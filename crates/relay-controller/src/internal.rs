//! `RelayInternal` service — the in-cluster-only surface.
//!
//! Reachable only by SA-token holders. The harness dials `Subscribe`,
//! the server-request methods, and `DeliverOutbound`. The listener
//! verifies the bearer SA token via TokenReview before any handler runs.

use std::sync::Arc;

use proto_common::{
    channel_stream_outbound, ChannelStreamOutbound, ChannelStreamSend,
    SendServerNotificationRequest, SendServerNotificationResponse,
    SendServerRequestAndAwaitRequest, SendServerRequestAndAwaitResponse, SubscribeRequest,
    TurnState, TurnStateEvent, UserMessage,
};
use relay_proto::relay_internal_server::RelayInternal;
use relay_proto::{DeliverStreamItemRequest, DeliverStreamItemResponse};
use shared::auth::{extract_bearer_token, TokenVerifier};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::state::GatewayState;

/// Default server-side wait cap for `SendServerRequestAndAwait` when the
/// caller passes 0.
const DEFAULT_AWAIT_SECS: u32 = 30;
/// Hard ceiling for the caller-supplied wait cap.
const MAX_AWAIT_SECS: u32 = 300;

pub struct InternalService {
    state: Arc<GatewayState>,
    /// Verifies the inbound SA token via TokenReview. `None` → no kube
    /// client is available and every authed RPC fails closed with
    /// FailedPrecondition.
    verifier: Option<Arc<dyn TokenVerifier>>,
}

impl InternalService {
    pub fn new(state: Arc<GatewayState>, verifier: Option<Arc<dyn TokenVerifier>>) -> Self {
        Self { state, verifier }
    }

    /// Verify the caller's SA token and return its agent. The internal
    /// listener authenticates by the bearer SA token alone.
    async fn verify_caller<T>(&self, request: &Request<T>) -> Result<String, Status> {
        let verifier = self.verifier.as_ref().ok_or_else(|| {
            Status::failed_precondition(
                "no token verifier configured: caller identity cannot be established",
            )
        })?;
        let token = extract_bearer_token(request)?;
        verifier.verify_token(token).await
    }
}

#[tonic::async_trait]
impl RelayInternal for InternalService {
    type SubscribeStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<UserMessage, Status>> + Send>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let agent = self.verify_caller(&request).await?;
        let mut rx = self.state.subscribe_or_create(&agent).await;
        let (tx, stream_rx) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Ok(msg) = rx.recv().await {
                if tx.send(Ok(msg)).await.is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(stream_rx))))
    }

    async fn send_server_notification(
        &self,
        request: Request<SendServerNotificationRequest>,
    ) -> Result<Response<SendServerNotificationResponse>, Status> {
        let agent = self.verify_caller(&request).await?;
        let req = request.into_inner();
        if req.method.is_empty() {
            return Err(Status::invalid_argument("method required"));
        }
        let delivered = match self
            .state
            .send_server_notification(&agent, &req.method, req.params_json)
            .await
        {
            Ok(()) => true,
            Err(_) => false,
        };
        Ok(Response::new(SendServerNotificationResponse { delivered }))
    }

    async fn send_server_request_and_await(
        &self,
        request: Request<SendServerRequestAndAwaitRequest>,
    ) -> Result<Response<SendServerRequestAndAwaitResponse>, Status> {
        let agent = self.verify_caller(&request).await?;
        let req = request.into_inner();
        if req.request_id.is_empty() {
            return Err(Status::invalid_argument(
                "request_id required (notifications use SendServerNotification)",
            ));
        }
        if req.method.is_empty() {
            return Err(Status::invalid_argument("method required"));
        }
        let timeout_secs = clamp_await_secs(req.timeout_seconds);
        match self
            .state
            .send_server_request_and_await(
                &agent,
                &req.request_id,
                &req.method,
                req.params_json,
                std::time::Duration::from_secs(timeout_secs as u64),
            )
            .await
        {
            Ok(crate::state::ServerRequestOutcome::Result(result_json)) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json,
                    error: None,
                    timed_out: false,
                    unknown_channel_stream: false,
                    unsupported_method: false,
                }))
            }
            Ok(crate::state::ServerRequestOutcome::Error(e)) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: Some(e),
                    timed_out: false,
                    unknown_channel_stream: false,
                    unsupported_method: false,
                }))
            }
            Err(crate::state::ServerRequestError::UnsupportedMethod) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: None,
                    timed_out: false,
                    unknown_channel_stream: false,
                    unsupported_method: true,
                }))
            }
            Err(crate::state::ServerRequestError::Timeout) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: None,
                    timed_out: true,
                    unknown_channel_stream: false,
                    unsupported_method: false,
                }))
            }
            Err(crate::state::ServerRequestError::UnknownChannelStream) | Err(_) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: None,
                    timed_out: false,
                    unknown_channel_stream: true,
                    unsupported_method: false,
                }))
            }
        }
    }

    async fn deliver_reply(
        &self,
        request: Request<relay_proto::DeliverReplyRequest>,
    ) -> Result<Response<relay_proto::DeliverReplyResponse>, Status> {
        let agent = self.verify_caller(&request).await?;
        let req = request.into_inner();
        tracing::info!(conversation_id = %req.conversation_id, agent = %agent, "deliver_reply received from harness");
        let send = ChannelStreamOutbound {
            command: Some(channel_stream_outbound::Command::SendMessage(
                ChannelStreamSend {
                    content: req.content,
                    conversation_id: req.conversation_id.clone(),
                },
            )),
        };
        let delivered = self.state.send_to_agent(&agent, send).await > 0;
        tracing::info!(conversation_id = %req.conversation_id, agent = %agent, delivered = delivered, "deliver_reply fanned out");
        Ok(Response::new(relay_proto::DeliverReplyResponse {
            delivered,
        }))
    }

    async fn deliver_turn_state(
        &self,
        request: Request<relay_proto::DeliverTurnStateRequest>,
    ) -> Result<Response<relay_proto::DeliverTurnStateResponse>, Status> {
        let agent = self.verify_caller(&request).await?;
        let req = request.into_inner();
        let Some(turn_state) = req.turn_state else {
            return Err(Status::invalid_argument("turn_state required"));
        };
        tracing::info!(conversation_id = %turn_state.conversation_id, agent = %agent, "deliver_turn_state received from harness");
        let subject = if req.subject.is_empty() {
            // Backward compat: resolve subject from channel_stream_id if provided
            match self
                .state
                .channel_stream_binding(&req.channel_stream_id)
                .await
            {
                Some((_, subject, bound)) if bound == agent => subject,
                _ => String::new(),
            }
        } else {
            req.subject
        };
        let state = TurnState::try_from(turn_state.state).unwrap_or(TurnState::Unspecified);

        // Record the turn state for poll recovery.
        if state == TurnState::Failed {
            self.state
                .record_turn_state(
                    &subject,
                    &turn_state.conversation_id,
                    crate::state::TurnStateRecord {
                        state,
                        reason: turn_state.reason.clone(),
                        code: turn_state.code.clone(),
                    },
                )
                .await;
        } else if !subject.is_empty() {
            self.state
                .record_turn_state(
                    &subject,
                    &turn_state.conversation_id,
                    crate::state::TurnStateRecord {
                        state,
                        reason: String::new(),
                        code: String::new(),
                    },
                )
                .await;
        }

        // Build the outbound frame and fan out to all agent channel streams.
        let conv_id = turn_state.conversation_id.clone();
        let msg = ChannelStreamOutbound {
            command: Some(channel_stream_outbound::Command::TurnState(
                TurnStateEvent {
                    state: turn_state.state,
                    conversation_id: turn_state.conversation_id,
                    reason: turn_state.reason,
                    code: turn_state.code,
                    ..Default::default()
                },
            )),
        };
        let delivered = self.state.send_to_agent(&agent, msg).await > 0;
        tracing::info!(conversation_id = %conv_id, agent = %agent, delivered = delivered, "deliver_turn_state fanned out");
        Ok(Response::new(relay_proto::DeliverTurnStateResponse {
            delivered,
        }))
    }

    // deliver_outbound is kept for backward compatibility but no longer called
    // by the harness. It's removed from the service trait so this is dead code.

    async fn deliver_stream_item(
        &self,
        request: Request<DeliverStreamItemRequest>,
    ) -> Result<Response<DeliverStreamItemResponse>, Status> {
        let agent = self.verify_caller(&request).await?;
        let req = request.into_inner();
        let Some(item) = req.item else {
            return Err(Status::invalid_argument("item required"));
        };
        // Pure relay: wrap the StreamItem verbatim, no payload inspection or
        // collapsing. The harness is the sole egress authority.
        let frame = ChannelStreamOutbound {
            command: Some(channel_stream_outbound::Command::StreamItem(item)),
        };
        let delivered = self.state.send_to_agent(&agent, frame).await > 0;
        Ok(Response::new(DeliverStreamItemResponse { delivered }))
    }
}

/// Clamp the caller-supplied wait cap: 0 → default; otherwise capped at
/// the ceiling. Extracted so the clamping is unit-testable.
fn clamp_await_secs(requested: u32) -> u32 {
    if requested == 0 {
        DEFAULT_AWAIT_SECS
    } else {
        requested.min(MAX_AWAIT_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use proto_common::{ContentBlock, TurnStateEvent};

    use crate::state::test_support::{make_state, mint_person};

    /// Stub verifier that always authenticates the caller as a fixed
    /// agent, so handler tests exercise the post-auth path without a
    /// kube client.
    struct FixedVerifier(String);

    #[async_trait]
    impl TokenVerifier for FixedVerifier {
        async fn verify_token(&self, _token: &str) -> Result<String, Status> {
            Ok(self.0.clone())
        }
    }

    fn service_for(state: Arc<GatewayState>, agent: &str) -> InternalService {
        InternalService::new(state, Some(Arc::new(FixedVerifier(agent.to_string()))))
    }

    fn authed<T>(message: T) -> Request<T> {
        let mut req = Request::new(message);
        req.metadata_mut()
            .insert("authorization", "Bearer token".parse().unwrap());
        req
    }

    #[test]
    fn clamp_await_defaults_zero_caps_the_ceiling_passes_the_rest() {
        assert_eq!(clamp_await_secs(0), DEFAULT_AWAIT_SECS);
        assert_eq!(clamp_await_secs(10_000), MAX_AWAIT_SECS);
        assert_eq!(clamp_await_secs(42), 42);
    }

    /// `subscribe` returns a streaming `Response` whose Ok variant is not
    /// `Debug`, so `unwrap_err` won't compile. Extract the error directly.
    fn subscribe_err(
        result: Result<Response<<InternalService as RelayInternal>::SubscribeStream>, Status>,
    ) -> Status {
        match result {
            Ok(_) => panic!("expected an error from subscribe"),
            Err(s) => s,
        }
    }

    #[tokio::test]
    async fn subscribe_without_verifier_fails_precondition() {
        let service = InternalService::new(make_state(), None);
        let err = subscribe_err(service.subscribe(authed(SubscribeRequest {})).await);
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn subscribe_missing_bearer_token_is_permission_denied() {
        let service = service_for(make_state(), "ag");
        // No authorization metadata.
        let err = subscribe_err(service.subscribe(Request::new(SubscribeRequest {})).await);
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn subscribe_delivers_notified_message() {
        let state = make_state();
        let service = service_for(state.clone(), "ag");
        let resp = service
            .subscribe(authed(SubscribeRequest {}))
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        state
            .notify_subscriber(
                "ag",
                UserMessage {
                    content: vec![],
                    sender: "u".into(),
                    reply_channel_stream: Some("chan".into()),
                    conversation_id: "ag.c".into(),
                },
            )
            .await;
        let msg = futures::StreamExt::next(&mut stream)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg.sender, "u");
    }

    #[tokio::test]
    async fn send_server_notification_unknown_channel_stream_reports_not_delivered() {
        let service = service_for(make_state(), "ag");
        let resp = service
            .send_server_notification(authed(SendServerNotificationRequest {
                method: "RevealPath".into(),
                params_json: "{}".into(),
                agent: "ag".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.delivered);
    }

    #[tokio::test]
    async fn send_server_request_unknown_channel_stream_sets_unknown_flag() {
        let service = service_for(make_state(), "ag");
        let resp = service
            .send_server_request_and_await(authed(SendServerRequestAndAwaitRequest {
                request_id: "r".into(),
                method: "AskUser".into(),
                params_json: "{}".into(),
                timeout_seconds: 1,
                agent: "ag".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.unknown_channel_stream);
    }

    #[tokio::test]
    async fn deliver_reply_fans_out_to_agent() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(8);
        let _id = mint_person(&state, "subject-ag", "ag", tx).await;
        let service = service_for(state, "ag");
        let resp = service
            .deliver_reply(authed(relay_proto::DeliverReplyRequest {
                conversation_id: "ag.conv".into(),
                content: vec![ContentBlock {
                    block: Some(proto_common::content_block::Block::Text(
                        proto_common::TextBlock { text: "hi".into() },
                    )),
                }],
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.delivered);
        let frame = rx.recv().await.unwrap();
        assert!(matches!(
            frame.command,
            Some(channel_stream_outbound::Command::SendMessage(_))
        ));
    }

    #[tokio::test]
    async fn deliver_turn_state_applies_working() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(8);
        let id = mint_person(&state, "subject-ag", "ag", tx).await;
        let service = service_for(state.clone(), "ag");
        let resp = service
            .deliver_turn_state(authed(relay_proto::DeliverTurnStateRequest {
                channel_stream_id: id.clone(),
                turn_state: Some(TurnStateEvent {
                    state: TurnState::Working as i32,
                    conversation_id: "ag.conv".into(),
                    reason: String::new(),
                    code: String::new(),
                    ..Default::default()
                }),
                subject: "subject-ag".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.delivered);
        let frame = rx.recv().await.unwrap();
        assert!(matches!(
            frame.command,
            Some(channel_stream_outbound::Command::TurnState(_))
        ));
        let rec = state
            .turn_state_record("subject-ag", "ag.conv")
            .await
            .unwrap();
        assert_eq!(rec.state, TurnState::Working);
    }

    #[tokio::test]
    async fn deliver_turn_state_failed_carries_reason_and_code() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(8);
        let id = mint_person(&state, "subject-ag", "ag", tx).await;
        let service = service_for(state.clone(), "ag");
        service
            .deliver_turn_state(authed(relay_proto::DeliverTurnStateRequest {
                channel_stream_id: id.clone(),
                turn_state: Some(TurnStateEvent {
                    state: TurnState::Failed as i32,
                    conversation_id: "ag.conv".into(),
                    reason: "inference job died".into(),
                    code: "14".into(),
                    ..Default::default()
                }),
                subject: "subject-ag".into(),
            }))
            .await
            .unwrap();
        let frame = rx.recv().await.unwrap();
        match frame.command {
            Some(channel_stream_outbound::Command::TurnState(e)) => {
                assert_eq!(e.state, TurnState::Failed as i32);
                assert_eq!(e.reason, "inference job died");
                assert_eq!(e.code, "14");
            }
            other => panic!("expected failed TurnState, got {other:?}"),
        }
        let rec = state
            .turn_state_record("subject-ag", "ag.conv")
            .await
            .unwrap();
        assert_eq!(rec.reason, "inference job died");
        assert_eq!(rec.code, "14");
    }

    #[tokio::test]
    async fn deliver_stream_item_wraps_verbatim_unchanged() {
        use proto_common::{item_start, stream_item, ItemStart, StreamItem, ToolUseItem};
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(8);
        let id = mint_person(&state, "subject-ag", "ag", tx).await;
        let service = service_for(state, "ag");
        // A tool-use start frame with a distinctive envelope; the gateway must
        // forward the StreamItem's bytes unchanged (no inspection/collapsing).
        let item = StreamItem {
            event_id: "ev-1".into(),
            item_id: "tc-1".into(),
            conversation_id: "ag.conv".into(),
            phase: Some(stream_item::Phase::Start(ItemStart {
                kind: Some(item_start::Kind::ToolUse(ToolUseItem {
                    name: "Bash".into(),
                })),
            })),
        };
        let resp = service
            .deliver_stream_item(authed(DeliverStreamItemRequest {
                channel_stream_id: id,
                item: Some(item.clone()),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.delivered);
        let frame = rx.recv().await.unwrap();
        match frame.command {
            Some(channel_stream_outbound::Command::StreamItem(got)) => {
                // Round-trip unchanged: the relayed item equals the input.
                assert_eq!(got, item);
            }
            other => panic!("expected StreamItem command, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deliver_stream_item_fans_out_to_agent_not_cross_agent() {
        let state = make_state();
        let (tx_other, mut rx_other) = mpsc::channel(4);
        // Channel stream owned by "other" agent.
        let _id = mint_person(&state, "subject-other", "other", tx_other).await;
        let service = service_for(state, "ag");
        // Verifier authenticates as "ag", so fan-out goes to "ag" channel streams only.
        // There are none, so nothing is delivered.
        let resp = service
            .deliver_stream_item(authed(DeliverStreamItemRequest {
                channel_stream_id: String::new(),
                item: Some(proto_common::StreamItem::default()),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.delivered);
        assert!(
            rx_other.try_recv().is_err(),
            "other agent channel stream must not receive"
        );
    }
}
