//! `TightbeamInternal` service — the in-cluster-only surface.
//!
//! Reachable only by SA-token holders. The transponder dials `Subscribe`
//! and the server-request methods; hangar dials `DeliverOutbound`. The
//! listener verifies the bearer SA token via TokenReview before any
//! handler runs.

use std::sync::Arc;

use proto_common::{
    channel_outbound, ChannelOutbound, ChannelSend, SendServerNotificationRequest,
    SendServerNotificationResponse, SendServerRequestAndAwaitRequest,
    SendServerRequestAndAwaitResponse, SubscribeRequest, TurnState, UserMessage,
};
use shared::auth::{extract_bearer_token, TokenVerifier};
use tightbeam_proto::tightbeam_internal_server::TightbeamInternal;
use tightbeam_proto::{
    DeliverOutboundRequest, DeliverOutboundResponse, DeliverStreamItemRequest,
    DeliverStreamItemResponse,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::state::{GatewayState, ServerRequestError, ServerRequestOutcome};

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

    /// Verify the caller's SA token and return its workspace. The internal
    /// listener has no signature middleware — the bearer SA token is the
    /// sole identity.
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
impl TightbeamInternal for InternalService {
    type SubscribeStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<UserMessage, Status>> + Send>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let workspace = self.verify_caller(&request).await?;
        let mut rx = self.state.subscribe_or_create(&workspace).await;
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
        let workspace = self.verify_caller(&request).await?;
        let req = request.into_inner();
        if req.channel_id.is_empty() {
            return Err(Status::invalid_argument("channel_id required"));
        }
        if req.method.is_empty() {
            return Err(Status::invalid_argument("method required"));
        }
        match self.state.channel_workspace(&req.channel_id).await {
            Some(bound) if bound == workspace => {}
            Some(_) => {
                return Err(Status::permission_denied(
                    "channel_id is bound to a different workspace",
                ));
            }
            None => {
                return Ok(Response::new(SendServerNotificationResponse {
                    delivered: false,
                }));
            }
        }
        let delivered = self
            .state
            .send_server_notification(&req.channel_id, &req.method, req.params_json)
            .await
            .is_ok();
        Ok(Response::new(SendServerNotificationResponse { delivered }))
    }

    async fn send_server_request_and_await(
        &self,
        request: Request<SendServerRequestAndAwaitRequest>,
    ) -> Result<Response<SendServerRequestAndAwaitResponse>, Status> {
        let workspace = self.verify_caller(&request).await?;
        let req = request.into_inner();
        if req.channel_id.is_empty() {
            return Err(Status::invalid_argument("channel_id required"));
        }
        if req.request_id.is_empty() {
            return Err(Status::invalid_argument(
                "request_id required (notifications use SendServerNotification)",
            ));
        }
        if req.method.is_empty() {
            return Err(Status::invalid_argument("method required"));
        }
        match self.state.channel_workspace(&req.channel_id).await {
            Some(bound) if bound == workspace => {}
            Some(_) => {
                return Err(Status::permission_denied(
                    "channel_id is bound to a different workspace",
                ));
            }
            None => {
                return Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: None,
                    timed_out: false,
                    unknown_channel: true,
                    unsupported_method: false,
                }));
            }
        }
        let secs = clamp_await_secs(req.timeout_seconds);
        let timeout = std::time::Duration::from_secs(secs as u64);
        match self
            .state
            .send_server_request_and_await(
                &req.channel_id,
                &req.request_id,
                &req.method,
                req.params_json,
                timeout,
            )
            .await
        {
            Ok(ServerRequestOutcome::Result(s)) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: s,
                    error: None,
                    timed_out: false,
                    unknown_channel: false,
                    unsupported_method: false,
                }))
            }
            Ok(ServerRequestOutcome::Error(e)) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: Some(e),
                    timed_out: false,
                    unknown_channel: false,
                    unsupported_method: false,
                }))
            }
            Err(ServerRequestError::Timeout) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: None,
                    timed_out: true,
                    unknown_channel: false,
                    unsupported_method: false,
                }))
            }
            Err(ServerRequestError::UnknownChannel) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: None,
                    timed_out: false,
                    unknown_channel: true,
                    unsupported_method: false,
                }))
            }
            Err(ServerRequestError::UnsupportedMethod) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: None,
                    timed_out: false,
                    unknown_channel: false,
                    unsupported_method: true,
                }))
            }
            Err(ServerRequestError::SendFailed) | Err(ServerRequestError::Disconnected) => Err(
                Status::aborted("channel disconnected before client responded"),
            ),
        }
    }

    async fn deliver_outbound(
        &self,
        request: Request<DeliverOutboundRequest>,
    ) -> Result<Response<DeliverOutboundResponse>, Status> {
        let workspace = self.verify_caller(&request).await?;
        let req = request.into_inner();
        if req.channel_id.is_empty() {
            return Err(Status::invalid_argument("channel_id required"));
        }
        match self.state.channel_workspace(&req.channel_id).await {
            Some(bound) if bound == workspace => {}
            Some(_) => {
                return Err(Status::permission_denied(
                    "channel_id is bound to a different workspace",
                ));
            }
            None => {
                return Ok(Response::new(DeliverOutboundResponse { delivered: false }));
            }
        }

        // Ordering guarantee: enqueue the reply BEFORE applying turn_state,
        // so a client that renders a SendMessage then a TurnState sees the
        // assistant reply land before the indicator clears — mirrors the old
        // hangar `send_to_channel`-then-`set_and_broadcast_turn_state` pair.
        let mut delivered = true;
        if let Some(reply) = req.reply {
            let send = ChannelOutbound {
                command: Some(channel_outbound::Command::SendMessage(ChannelSend {
                    content: reply.content,
                    conversation_id: req.conversation_id.clone(),
                })),
            };
            delivered &= self.state.send_to_channel(&req.channel_id, send).await;
        }

        if let Some(turn_state) = req.turn_state {
            let state = TurnState::try_from(turn_state.state).unwrap_or(TurnState::Unspecified);
            let applied = if state == TurnState::Failed {
                self.state
                    .set_and_broadcast_turn_failed(
                        &req.channel_id,
                        &workspace,
                        &turn_state.conversation_id,
                        &turn_state.reason,
                        &turn_state.code,
                    )
                    .await
            } else {
                self.state
                    .set_and_broadcast_turn_state(
                        &req.channel_id,
                        &workspace,
                        &turn_state.conversation_id,
                        state,
                    )
                    .await
            };
            delivered &= applied;
        }

        Ok(Response::new(DeliverOutboundResponse { delivered }))
    }

    async fn deliver_stream_item(
        &self,
        request: Request<DeliverStreamItemRequest>,
    ) -> Result<Response<DeliverStreamItemResponse>, Status> {
        let workspace = self.verify_caller(&request).await?;
        let req = request.into_inner();
        if req.channel_id.is_empty() {
            return Err(Status::invalid_argument("channel_id required"));
        }
        match self.state.channel_workspace(&req.channel_id).await {
            Some(bound) if bound == workspace => {}
            Some(_) => {
                return Err(Status::permission_denied(
                    "channel_id is bound to a different workspace",
                ));
            }
            None => {
                return Ok(Response::new(DeliverStreamItemResponse { delivered: false }));
            }
        }
        let Some(item) = req.item else {
            return Err(Status::invalid_argument("item required"));
        };
        // Pure relay: wrap the StreamItem verbatim, no payload inspection or
        // collapsing. The transponder is the sole egress authority.
        let frame = ChannelOutbound {
            command: Some(channel_outbound::Command::StreamItem(item)),
        };
        let delivered = self.state.send_to_channel(&req.channel_id, frame).await;
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
    use shared::client_signature::ClientSignatureVerifier;
    use std::time::Duration;

    /// Stub verifier that always authenticates the caller as a fixed
    /// workspace, so handler tests exercise the post-auth path without a
    /// kube client.
    struct FixedVerifier(String);

    #[async_trait]
    impl TokenVerifier for FixedVerifier {
        async fn verify_token(&self, _token: &str) -> Result<String, Status> {
            Ok(self.0.clone())
        }
    }

    fn make_state() -> Arc<GatewayState> {
        Arc::new(GatewayState::new(
            Arc::new(ClientSignatureVerifier::new(Duration::from_secs(300))),
            ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng),
            None,
            "default".into(),
        ))
    }

    fn service_for(state: Arc<GatewayState>, workspace: &str) -> InternalService {
        InternalService::new(state, Some(Arc::new(FixedVerifier(workspace.to_string()))))
    }

    fn authed<T>(message: T) -> Request<T> {
        let mut req = Request::new(message);
        req.metadata_mut()
            .insert("authorization", "Bearer token".parse().unwrap());
        req
    }

    #[test]
    fn clamp_await_zero_returns_default() {
        assert_eq!(clamp_await_secs(0), DEFAULT_AWAIT_SECS);
    }

    #[test]
    fn clamp_await_caps_at_ceiling() {
        assert_eq!(clamp_await_secs(10_000), MAX_AWAIT_SECS);
    }

    #[test]
    fn clamp_await_passes_through_in_range() {
        assert_eq!(clamp_await_secs(42), 42);
    }

    /// `subscribe` returns a streaming `Response` whose Ok variant is not
    /// `Debug`, so `unwrap_err` won't compile. Extract the error directly.
    fn subscribe_err(
        result: Result<Response<<InternalService as TightbeamInternal>::SubscribeStream>, Status>,
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
        let service = service_for(make_state(), "ws");
        // No authorization metadata.
        let err = subscribe_err(service.subscribe(Request::new(SubscribeRequest {})).await);
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    // The internal listener authenticates ONLY K8s SA tokens. Client-signature
    // metadata (x-sig-*) is inert here — a request carrying only x-sig-* headers
    // and no bearer token is still PermissionDenied.
    //
    // This locks "x-sig-* is not an accepted credential today"; it does NOT
    // prove a future signature verifier couldn't be wired in.
    // That guarantee is structural (InternalService holds only a TokenVerifier,
    // never a ClientSignatureVerifier) and is enforced by review, not this test.
    #[tokio::test]
    async fn internal_door_ignores_client_signature_metadata() {
        let service = service_for(make_state(), "ws");
        let mut req = Request::new(SubscribeRequest {});
        // Client-signature envelope headers, but NO `authorization` bearer token.
        req.metadata_mut()
            .insert("x-sig-kid", "client-alpha".parse().unwrap());
        req.metadata_mut()
            .insert("x-sig-signature", "deadbeef".parse().unwrap());
        let err = subscribe_err(service.subscribe(req).await);
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn subscribe_delivers_notified_message() {
        let state = make_state();
        let service = service_for(state.clone(), "ws");
        let resp = service
            .subscribe(authed(SubscribeRequest {}))
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        state
            .notify_subscriber(
                "ws",
                UserMessage {
                    content: vec![],
                    sender: "u".into(),
                    reply_channel: Some("chan".into()),
                    conversation_id: "ws.c".into(),
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
    async fn send_server_notification_unknown_channel_reports_not_delivered() {
        let service = service_for(make_state(), "ws");
        let resp = service
            .send_server_notification(authed(SendServerNotificationRequest {
                channel_id: "ghost".into(),
                method: "RevealPath".into(),
                params_json: "{}".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.delivered);
    }

    #[tokio::test]
    async fn send_server_notification_cross_workspace_channel_denied() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(4);
        // Channel owned by "other".
        let id = state.mint_channel("other".into(), None, tx).await;
        let service = service_for(state, "ws");
        let err = service
            .send_server_notification(authed(SendServerNotificationRequest {
                channel_id: id,
                method: "RevealPath".into(),
                params_json: "{}".into(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn send_server_request_unknown_channel_sets_unknown_flag() {
        let service = service_for(make_state(), "ws");
        let resp = service
            .send_server_request_and_await(authed(SendServerRequestAndAwaitRequest {
                channel_id: "ghost".into(),
                request_id: "r".into(),
                method: "AskUser".into(),
                params_json: "{}".into(),
                timeout_seconds: 1,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.unknown_channel);
    }

    #[tokio::test]
    async fn send_server_request_cross_workspace_channel_denied() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(4);
        // Channel owned by "other"; caller authenticates as "ws".
        let id = state.mint_channel("other".into(), None, tx).await;
        let service = service_for(state, "ws");
        let err = service
            .send_server_request_and_await(authed(SendServerRequestAndAwaitRequest {
                channel_id: id,
                request_id: "r".into(),
                method: "AskUser".into(),
                params_json: "{}".into(),
                timeout_seconds: 1,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn deliver_outbound_enqueues_reply_then_turn_state_in_order() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(8);
        let id = state.mint_channel("ws".into(), None, tx).await;
        let service = service_for(state, "ws");
        let resp = service
            .deliver_outbound(authed(DeliverOutboundRequest {
                channel_id: id.clone(),
                conversation_id: "ws.conv".into(),
                reply: Some(tightbeam_proto::ChannelReply {
                    content: vec![ContentBlock {
                        block: Some(proto_common::content_block::Block::Text(
                            proto_common::TextBlock { text: "hi".into() },
                        )),
                    }],
                }),
                turn_state: Some(TurnStateEvent {
                    state: TurnState::Idle as i32,
                    conversation_id: "ws.conv".into(),
                    reason: String::new(),
                    code: String::new(),
                }),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.delivered);
        // First frame must be the SendMessage reply, then the TurnState.
        let first = rx.recv().await.unwrap();
        assert!(
            matches!(
                first.command,
                Some(channel_outbound::Command::SendMessage(_))
            ),
            "reply must be enqueued before turn_state"
        );
        let second = rx.recv().await.unwrap();
        match second.command {
            Some(channel_outbound::Command::TurnState(e)) => {
                assert_eq!(e.state, TurnState::Idle as i32);
            }
            other => panic!("expected TurnState second, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deliver_outbound_turn_state_only_applies_without_reply() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(8);
        let id = state.mint_channel("ws".into(), None, tx).await;
        let service = service_for(state.clone(), "ws");
        let resp = service
            .deliver_outbound(authed(DeliverOutboundRequest {
                channel_id: id.clone(),
                conversation_id: "ws.conv".into(),
                reply: None,
                turn_state: Some(TurnStateEvent {
                    state: TurnState::Working as i32,
                    conversation_id: "ws.conv".into(),
                    reason: String::new(),
                    code: String::new(),
                }),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.delivered);
        let frame = rx.recv().await.unwrap();
        assert!(matches!(
            frame.command,
            Some(channel_outbound::Command::TurnState(_))
        ));
        let rec = state.turn_state_record("ws", "ws.conv").await.unwrap();
        assert_eq!(rec.state, TurnState::Working);
    }

    #[tokio::test]
    async fn deliver_outbound_failed_state_carries_reason_and_code() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(8);
        let id = state.mint_channel("ws".into(), None, tx).await;
        let service = service_for(state.clone(), "ws");
        service
            .deliver_outbound(authed(DeliverOutboundRequest {
                channel_id: id.clone(),
                conversation_id: "ws.conv".into(),
                reply: None,
                turn_state: Some(TurnStateEvent {
                    state: TurnState::Failed as i32,
                    conversation_id: "ws.conv".into(),
                    reason: "worker died".into(),
                    code: "14".into(),
                }),
            }))
            .await
            .unwrap();
        let frame = rx.recv().await.unwrap();
        match frame.command {
            Some(channel_outbound::Command::TurnState(e)) => {
                assert_eq!(e.state, TurnState::Failed as i32);
                assert_eq!(e.reason, "worker died");
                assert_eq!(e.code, "14");
            }
            other => panic!("expected failed TurnState, got {other:?}"),
        }
        let rec = state.turn_state_record("ws", "ws.conv").await.unwrap();
        assert_eq!(rec.reason, "worker died");
        assert_eq!(rec.code, "14");
    }

    #[tokio::test]
    async fn deliver_outbound_unknown_channel_reports_not_delivered() {
        let service = service_for(make_state(), "ws");
        let resp = service
            .deliver_outbound(authed(DeliverOutboundRequest {
                channel_id: "ghost".into(),
                conversation_id: "ws.c".into(),
                reply: None,
                turn_state: None,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.delivered);
    }

    #[tokio::test]
    async fn deliver_outbound_reply_send_failure_reports_not_delivered() {
        let state = make_state();
        let (tx, rx) = mpsc::channel(8);
        let id = state.mint_channel("ws".into(), None, tx).await;
        // Drop the receiver so the reply send fails; the channel stays registered.
        drop(rx);
        let service = service_for(state, "ws");
        let resp = service
            .deliver_outbound(authed(DeliverOutboundRequest {
                channel_id: id,
                conversation_id: "ws.conv".into(),
                reply: Some(tightbeam_proto::ChannelReply { content: vec![] }),
                turn_state: None,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.delivered);
    }

    #[tokio::test]
    async fn deliver_outbound_turn_state_send_failure_reports_not_delivered() {
        let state = make_state();
        let (tx, rx) = mpsc::channel(8);
        let id = state.mint_channel("ws".into(), None, tx).await;
        // Drop the receiver so the turn-state broadcast fails; the channel stays registered.
        drop(rx);
        let service = service_for(state, "ws");
        let resp = service
            .deliver_outbound(authed(DeliverOutboundRequest {
                channel_id: id,
                conversation_id: "ws.conv".into(),
                reply: None,
                turn_state: Some(TurnStateEvent {
                    state: TurnState::Idle as i32,
                    conversation_id: "ws.conv".into(),
                    reason: String::new(),
                    code: String::new(),
                }),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.delivered);
    }

    #[tokio::test]
    async fn deliver_stream_item_wraps_verbatim_unchanged() {
        use proto_common::{
            item_start, stream_item, ItemStart, StreamItem, ToolUseItem,
        };
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(8);
        let id = state.mint_channel("ws".into(), None, tx).await;
        let service = service_for(state, "ws");
        // A tool-use start frame with a distinctive envelope; the gateway must
        // forward the StreamItem's bytes unchanged (no inspection/collapsing).
        let item = StreamItem {
            workspace_seq: 7,
            event_id: "ev-1".into(),
            item_id: "tc-1".into(),
            conversation_id: "ws.conv".into(),
            phase: Some(stream_item::Phase::Start(ItemStart {
                kind: Some(item_start::Kind::ToolUse(ToolUseItem {
                    name: "Bash".into(),
                })),
            })),
        };
        let resp = service
            .deliver_stream_item(authed(DeliverStreamItemRequest {
                channel_id: id,
                item: Some(item.clone()),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.delivered);
        let frame = rx.recv().await.unwrap();
        match frame.command {
            Some(channel_outbound::Command::StreamItem(got)) => {
                // Round-trip unchanged: the relayed item equals the input.
                assert_eq!(got, item);
            }
            other => panic!("expected StreamItem command, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deliver_stream_item_unknown_channel_reports_not_delivered() {
        let service = service_for(make_state(), "ws");
        let resp = service
            .deliver_stream_item(authed(DeliverStreamItemRequest {
                channel_id: "ghost".into(),
                item: Some(proto_common::StreamItem::default()),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.delivered);
    }

    #[tokio::test]
    async fn deliver_stream_item_cross_workspace_channel_denied() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(4);
        let id = state.mint_channel("other".into(), None, tx).await;
        let service = service_for(state, "ws");
        let err = service
            .deliver_stream_item(authed(DeliverStreamItemRequest {
                channel_id: id,
                item: Some(proto_common::StreamItem::default()),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn deliver_outbound_cross_workspace_channel_denied() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(4);
        let id = state.mint_channel("other".into(), None, tx).await;
        let service = service_for(state, "ws");
        let err = service
            .deliver_outbound(authed(DeliverOutboundRequest {
                channel_id: id,
                conversation_id: "other.c".into(),
                reply: None,
                turn_state: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }
}
