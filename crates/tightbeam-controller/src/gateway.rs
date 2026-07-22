//! `TightbeamGateway` service — the signature-verified external surface.
//!
//! Two flavors of handler:
//!
//! - **Local-state handlers** own the live channel/subscriber/turn-state
//!   surface (channel_ingest, channel_receive, get_turn_state,
//!   redeem_enrollment, list_workspaces).
//! - **Forwarding handlers** (mint_conversation, list_conversations,
//!   delete_conversation, set_conversation_name, get_conversation_history,
//!   watch_tools, call_tool) carry no local state: the durable conversation
//!   log lives in the per-workspace transponder, so they pick the caller's
//!   transponder from the pool and pass the call through. Authorization is
//!   established here from the signature extension; the forwarded SA token
//!   authenticates tightbeam to the transponder, and per-workspace routing
//!   plus the transponder's own ownership check enforce isolation.

use std::sync::Arc;

use futures::StreamExt;
use proto_common::{
    channel_outbound, CallToolRequest, CallToolResponse, CancelTurnRequest, CancelTurnResponse,
    ChannelAck, ChannelIngestAck, ChannelIngestRequest, ChannelOutbound, ChannelReceiveRequest,
    DeleteConversationRequest, DeleteConversationResponse, GetConversationHistoryRequest,
    GetConversationHistoryResponse, GetTurnStateRequest, ListConversationsRequest,
    ListConversationsResponse, ListWorkspacesRequest, ListWorkspacesResponse,
    MintConversationRequest, MintConversationResponse, RedeemEnrollmentRequest,
    RedeemEnrollmentResponse, SetConversationNameRequest, SetConversationNameResponse,
    ToolListUpdate, TurnState, TurnStateEvent, UserMessage, WatchToolsRequest,
};
use tightbeam_proto::tightbeam_gateway_server::TightbeamGateway;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::state::GatewayState;

/// Seconds to keep the channel's outbound side open after the client
/// half-closes. Just under the 60s default gRPC client deadline.
const CHANNEL_DRAIN_SECS: u64 = 55;

/// Server-side clamp ceiling for `SetConversationName`. Enforced before
/// the forward so a hostile name never reaches the transponder.
const MAX_CONVERSATION_NAME_CHARS: usize = 200;

/// Fires `unregister_channel` after a drain delay when the held
/// outbound stream is dropped. Used by `channel_receive` to tie
/// channel-registry lifetime to the gRPC response stream's lifetime
/// instead of a wall-clock timer. The drain delay lets multi-frame
/// outbound replies that were queued at the moment of drop finish.
struct ChannelDropGuard {
    state: Arc<GatewayState>,
    channel_id: String,
}

impl Drop for ChannelDropGuard {
    fn drop(&mut self) {
        let state = self.state.clone();
        let channel_id = std::mem::take(&mut self.channel_id);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(CHANNEL_DRAIN_SECS)).await;
            tracing::info!(
                channel_id = %channel_id,
                "channel_receive: drain elapsed after stream drop, unregistering channel"
            );
            state.unregister_channel(&channel_id).await;
        });
    }
}

/// Wraps an outbound stream with a `ChannelDropGuard`. When tonic drops
/// the response stream (client disconnect or request cancellation), the
/// guard's `Drop` schedules the drain-and-unregister task. While the
/// stream is alive, the channel stays registered indefinitely.
struct GuardedStream<S> {
    inner: S,
    _guard: ChannelDropGuard,
}

impl<S: futures::Stream + Unpin> futures::Stream for GuardedStream<S> {
    type Item = S::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.inner).poll_next(cx)
    }
}

pub struct GatewayService {
    state: Arc<GatewayState>,
}

impl GatewayService {
    pub fn new(state: Arc<GatewayState>) -> Self {
        Self { state }
    }

    /// Read the workspace the signature middleware verified and stamped on
    /// the request extensions. The external listener trusts this entirely
    /// — the `SignatureLayer` ran first and populated it.
    #[allow(clippy::result_large_err)]
    fn verified_workspace<T>(request: &Request<T>) -> Result<String, Status> {
        request
            .extensions()
            .get::<crate::signature_layer::VerifiedWorkspace>()
            .map(|w| w.0.clone())
            .ok_or_else(|| {
                Status::permission_denied(
                    "missing verified workspace extension; middleware must populate it",
                )
            })
    }
}

/// True if a list_conversations request body's `workspace` field conflicts
/// with the verified workspace claim. An empty body field is accepted
/// (informational only); a non-empty body field MUST equal the claim.
fn workspace_claim_conflicts(body_ws: &str, verified_ws: &str) -> bool {
    !body_ws.is_empty() && body_ws != verified_ws
}

/// True when a `ClientResponse` delivery found no matching awaiter, so the
/// handler should warn-log.
fn warn_on_undelivered(delivered: bool) -> bool {
    !delivered
}

#[tonic::async_trait]
impl TightbeamGateway for GatewayService {
    async fn redeem_enrollment(
        &self,
        request: Request<RedeemEnrollmentRequest>,
    ) -> Result<Response<RedeemEnrollmentResponse>, Status> {
        // Unauthenticated by design — the signed enrollment code IS the
        // authentication artifact. Business logic + single-use guard live
        // in `enrollment_store::redeem_for_enrollment` so the
        // security-critical branches are unit-tested behind the
        // `EnrollmentStore` interface.
        let req = request.into_inner();
        let claims = crate::enrollment::verify_enrollment_code(
            &self.state.signing_key().verifying_key(),
            &req.enrollment_code,
        )?;

        let kube_client = self
            .state
            .kube_client()
            .ok_or_else(|| Status::failed_precondition("controller has no kube client"))?
            .clone();
        let store =
            crate::enrollment_store::KubeEnrollmentStore::new(kube_client, self.state.namespace());
        // Install the key into the verifier cache synchronously so this
        // device's immediate signed follow-up verifies without waiting
        // for the enrollment watcher's async install.
        let resp = crate::enrollment_store::redeem_and_install(
            &store,
            self.state.enrollment_verifier().registrations(),
            &claims,
            &req.public_key,
        )
        .await?;

        tracing::info!(
            workspace = %claims.workspace,
            enrollment = %resp.client_name,
            "enrollment redeemed"
        );

        Ok(Response::new(resp))
    }

    async fn list_workspaces(
        &self,
        request: Request<ListWorkspacesRequest>,
    ) -> Result<Response<ListWorkspacesResponse>, Status> {
        let kid = request
            .extensions()
            .get::<crate::signature_layer::VerifiedClient>()
            .map(|c| c.0.clone())
            .ok_or_else(|| {
                Status::permission_denied(
                    "missing verified client extension; middleware must populate it",
                )
            })?;
        let workspaces = self
            .state
            .enrollment_verifier()
            .get_workspaces_for_kid(&kid)
            .await
            .ok_or_else(|| Status::permission_denied("enrollment registration not found"))?;
        Ok(Response::new(ListWorkspacesResponse { workspaces }))
    }

    async fn mint_conversation(
        &self,
        request: Request<MintConversationRequest>,
    ) -> Result<Response<MintConversationResponse>, Status> {
        // Authorization is established by the signature middleware; the
        // verified workspace picks the transponder that owns this caller's
        // conversation log.
        let workspace = Self::verified_workspace(&request)?;
        let req = request.into_inner();
        let mut client = self
            .state
            .transponder_clients()
            .get(&workspace)
            .await
            .map_err(|e| Status::unavailable(format!("transponder unavailable: {e}")))?;
        client.as_mut().mint_conversation(req).await
    }

    async fn list_conversations(
        &self,
        request: Request<ListConversationsRequest>,
    ) -> Result<Response<ListConversationsResponse>, Status> {
        let workspace = Self::verified_workspace(&request)?;
        let req = request.into_inner();
        if workspace_claim_conflicts(&req.workspace, &workspace) {
            return Err(Status::permission_denied(
                "workspace claim does not match request body",
            ));
        }
        let mut client = self
            .state
            .transponder_clients()
            .get(&workspace)
            .await
            .map_err(|e| Status::unavailable(format!("transponder unavailable: {e}")))?;
        client.as_mut().list_conversations(req).await
    }

    async fn delete_conversation(
        &self,
        request: Request<DeleteConversationRequest>,
    ) -> Result<Response<DeleteConversationResponse>, Status> {
        let workspace = Self::verified_workspace(&request)?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "DeleteConversationRequest.conversation_id required",
            ));
        }
        let mut client = self
            .state
            .transponder_clients()
            .get(&workspace)
            .await
            .map_err(|e| Status::unavailable(format!("transponder unavailable: {e}")))?;
        client.as_mut().delete_conversation(req).await
    }

    async fn cancel_turn(
        &self,
        request: Request<CancelTurnRequest>,
    ) -> Result<Response<CancelTurnResponse>, Status> {
        let workspace = Self::verified_workspace(&request)?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "CancelTurnRequest.conversation_id required",
            ));
        }
        let mut client = self
            .state
            .transponder_clients()
            .get(&workspace)
            .await
            .map_err(|e| Status::unavailable(format!("transponder unavailable: {e}")))?;
        client.as_mut().cancel_turn(req).await
    }

    async fn set_conversation_name(
        &self,
        request: Request<SetConversationNameRequest>,
    ) -> Result<Response<SetConversationNameResponse>, Status> {
        let workspace = Self::verified_workspace(&request)?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "SetConversationNameRequest.conversation_id required",
            ));
        }
        require_conversation_name_within_limit(&req.name)?;
        let mut client = self
            .state
            .transponder_clients()
            .get(&workspace)
            .await
            .map_err(|e| Status::unavailable(format!("transponder unavailable: {e}")))?;
        client.as_mut().set_conversation_name(req).await
    }

    async fn get_conversation_history(
        &self,
        request: Request<GetConversationHistoryRequest>,
    ) -> Result<Response<GetConversationHistoryResponse>, Status> {
        let workspace = Self::verified_workspace(&request)?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument("conversation_id required"));
        }
        let mut client = self
            .state
            .transponder_clients()
            .get(&workspace)
            .await
            .map_err(|e| Status::unavailable(format!("transponder unavailable: {e}")))?;
        client.as_mut().get_conversation_history(req).await
    }

    async fn get_turn_state(
        &self,
        request: Request<GetTurnStateRequest>,
    ) -> Result<Response<TurnStateEvent>, Status> {
        // Read-only poll of the gateway-owned per-conversation turn phase,
        // scoped to the caller's verified workspace so it can only read its
        // own. Never dispatches to the LLM — it only reflects state the
        // gateway recorded on each turn-state broadcast.
        let workspace = Self::verified_workspace(&request)?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument("conversation_id required"));
        }
        // Unknown / never-active conversation → IDLE. Absence of a record
        // is not an error — it is exactly the "input enabled, nothing in
        // flight" state the client wants.
        let event = match self
            .state
            .turn_state_record(&workspace, &req.conversation_id)
            .await
        {
            Some(rec) => TurnStateEvent {
                state: rec.state as i32,
                conversation_id: req.conversation_id,
                reason: rec.reason,
                code: rec.code,
                ..Default::default()
            },
            None => TurnStateEvent {
                state: TurnState::Idle as i32,
                conversation_id: req.conversation_id,
                reason: String::new(),
                code: String::new(),
                ..Default::default()
            },
        };
        Ok(Response::new(event))
    }

    async fn channel_ingest(
        &self,
        request: Request<ChannelIngestRequest>,
    ) -> Result<Response<ChannelIngestAck>, Status> {
        // Workspace is derived from the caller's signature — NEVER from the
        // request body. The caller echoes the server-minted channel_id
        // received earlier from ChannelReceive's first ChannelAck frame; we
        // verify it belongs to the caller's verified workspace before
        // routing.
        let workspace = Self::verified_workspace(&request)?;
        let req = request.into_inner();
        if req.channel_id.is_empty() {
            return Err(Status::invalid_argument(
                "ChannelIngestRequest.channel_id required",
            ));
        }
        match self.state.channel_workspace(&req.channel_id).await {
            Some(bound) if bound == workspace => {}
            Some(_) => {
                return Err(Status::permission_denied(
                    "ChannelIngestRequest.channel_id is bound to a different workspace",
                ));
            }
            None => {
                return Err(Status::not_found(
                    "ChannelIngestRequest.channel_id is not registered (call ChannelReceive first)",
                ));
            }
        }

        // Capability set update is independent of the payload — clients
        // re-advertise on every ingest so a device switch picks up the
        // new renderer set immediately.
        if !req.supported_methods.is_empty() {
            self.state
                .update_supported_methods(&req.channel_id, req.supported_methods.clone())
                .await;
        }

        // Discriminate payload. Exactly one of user_message /
        // client_response must be set.
        match (req.user_message, req.client_response) {
            (Some(_), Some(_)) => Err(Status::invalid_argument(
                "ChannelIngestRequest must carry exactly one of user_message or client_response",
            )),
            (None, None) => Err(Status::invalid_argument(
                "ChannelIngestRequest must carry user_message or client_response",
            )),
            (Some(user_message), None) => {
                // Conversation id: empty → mint a fresh one via the caller's
                // transponder; non-empty (opaque UUID) → continue it.
                let conversation_id = if req.conversation_id.is_empty() {
                    let mut client = self
                        .state
                        .transponder_clients()
                        .get(&workspace)
                        .await
                        .map_err(|e| {
                            Status::unavailable(format!("transponder unavailable: {e}"))
                        })?;
                    client
                        .as_mut()
                        .mint_conversation(MintConversationRequest {})
                        .await?
                        .into_inner()
                        .conversation_id
                } else {
                    req.conversation_id.clone()
                };
                self.state
                    .notify_subscriber(
                        &workspace,
                        UserMessage {
                            content: user_message.content,
                            sender: user_message.sender,
                            reply_channel: Some(req.channel_id.clone()),
                            conversation_id: conversation_id.clone(),
                        },
                    )
                    .await;
                // The user message has been accepted and routed; the
                // transponder will pick it up momentarily. Move the channel
                // to WORKING so the client renders an active indicator until
                // the assistant message (or a TurnError) lands.
                self.state
                    .set_and_broadcast_turn_state(
                        &req.channel_id,
                        &workspace,
                        &conversation_id,
                        TurnState::Working,
                    )
                    .await;
                Ok(Response::new(ChannelIngestAck {
                    channel_id: req.channel_id,
                    conversation_id,
                }))
            }
            (None, Some(cr)) => {
                // Validate ClientResponse: exactly one of result_json / error.
                let result_set = !cr.result_json.is_empty();
                let error_set = cr.error.is_some();
                if result_set && error_set {
                    return Err(Status::invalid_argument(
                        "ClientResponse must carry exactly one of result_json or error",
                    ));
                }
                if !result_set && !error_set {
                    return Err(Status::invalid_argument(
                        "ClientResponse must carry result_json or error",
                    ));
                }
                let outcome = if error_set {
                    crate::state::ServerRequestOutcome::Error(cr.error.unwrap())
                } else {
                    crate::state::ServerRequestOutcome::Result(cr.result_json)
                };
                // Delivery to an unknown / expired request_id is benign
                // (timeout already fired). Warn-log but do not error.
                if warn_on_undelivered(
                    self.state
                        .deliver_client_response(&req.channel_id, &cr.request_id, outcome)
                        .await,
                ) {
                    tracing::warn!(
                        channel_id = %req.channel_id,
                        request_id = %cr.request_id,
                        "channel_ingest: ClientResponse for unknown/expired request_id"
                    );
                }
                // ClientResponse path does not touch a conversation — empty
                // id in the ack is the honest signal that no conversation
                // was routed by this call.
                Ok(Response::new(ChannelIngestAck {
                    channel_id: req.channel_id,
                    conversation_id: String::new(),
                }))
            }
        }
    }

    type ChannelReceiveStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChannelOutbound, Status>> + Send>>;

    async fn channel_receive(
        &self,
        request: Request<ChannelReceiveRequest>,
    ) -> Result<Response<Self::ChannelReceiveStream>, Status> {
        let workspace = Self::verified_workspace(&request)?;
        let req = request.into_inner();

        let (tx, rx) = mpsc::channel(16);
        // Mint the server-side channel_id, bind it to the verified
        // workspace, and stash the adapter_hint for log emission.
        let channel_id = self
            .state
            .mint_channel(workspace.clone(), req.adapter_hint.clone(), tx.clone())
            .await;
        tracing::info!(
            channel_id = %channel_id,
            workspace = %workspace,
            adapter_hint = ?req.adapter_hint,
            "channel_receive: minted channel_id"
        );

        // First frame of the outbound stream IS the ChannelAck — the
        // adapter MUST consume this before it can echo channel_id on
        // ChannelIngest.
        if let Err(e) = tx
            .send(ChannelOutbound {
                command: Some(channel_outbound::Command::Ack(ChannelAck {
                    channel_id: channel_id.clone(),
                })),
            })
            .await
        {
            tracing::warn!(?e, "channel_receive: failed to send initial ChannelAck");
        }
        // Second frame: replay the current turn phase so reconnects and
        // future second-device opens land in the correct visual state
        // immediately. Brand-new channels just minted above are IDLE.
        self.state.replay_turn_state(&channel_id).await;

        // Tie unregister to the outbound stream's lifetime, not a
        // wall-clock timer. The guard travels with the response stream;
        // tonic drops the stream on client disconnect or cancellation,
        // and only then is the drain-and-unregister task scheduled.
        let guard = ChannelDropGuard {
            state: self.state.clone(),
            channel_id: channel_id.clone(),
        };

        #[allow(clippy::result_large_err)]
        let outbound_stream =
            ReceiverStream::new(rx).map(|msg| -> Result<ChannelOutbound, Status> { Ok(msg) });
        let guarded = GuardedStream {
            inner: outbound_stream,
            _guard: guard,
        };

        Ok(Response::new(Box::pin(guarded)))
    }

    type WatchToolsStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<ToolListUpdate, Status>> + Send>>;

    async fn watch_tools(
        &self,
        request: Request<WatchToolsRequest>,
    ) -> Result<Response<Self::WatchToolsStream>, Status> {
        let workspace = Self::verified_workspace(&request)?;
        let mut client = self
            .state
            .transponder_clients()
            .get(&workspace)
            .await
            .map_err(|e| Status::unavailable(format!("transponder unavailable: {e}")))?;
        let upstream = client
            .as_mut()
            .watch_tools(WatchToolsRequest {})
            .await?
            .into_inner();
        // 1:1 pass-through. tonic Streaming<T> is already a
        // futures::Stream<Item = Result<T, Status>>.
        Ok(Response::new(Box::pin(upstream)))
    }

    async fn call_tool(
        &self,
        request: Request<CallToolRequest>,
    ) -> Result<Response<CallToolResponse>, Status> {
        let workspace = Self::verified_workspace(&request)?;
        let req = request.into_inner();
        let mut client = self
            .state
            .transponder_clients()
            .get(&workspace)
            .await
            .map_err(|e| Status::unavailable(format!("transponder unavailable: {e}")))?;
        client.as_mut().call_tool(req).await
    }
}

/// Reject a conversation name longer than `MAX_CONVERSATION_NAME_CHARS`
/// Unicode scalar values, before the transponder forward.
#[allow(clippy::result_large_err)]
fn require_conversation_name_within_limit(name: &str) -> Result<(), Status> {
    if name.chars().count() > MAX_CONVERSATION_NAME_CHARS {
        Err(Status::invalid_argument(format!(
            "name exceeds {MAX_CONVERSATION_NAME_CHARS}-character limit",
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature_layer::{VerifiedClient, VerifiedWorkspace};
    use shared::client_signature::{ClientRegistration, ClientSignatureVerifier};
    use std::time::Duration;

    fn fixture_signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng)
    }

    fn fixture_verifier() -> Arc<ClientSignatureVerifier> {
        Arc::new(ClientSignatureVerifier::new(Duration::from_secs(300)))
    }

    fn make_service_with(verifier: Arc<ClientSignatureVerifier>) -> GatewayService {
        let state = Arc::new(GatewayState::new(
            verifier,
            fixture_signing_key(),
            None,
            "default".into(),
        ));
        GatewayService::new(state)
    }

    fn fresh_vk() -> p256::ecdsa::VerifyingKey {
        use p256::ecdsa::SigningKey;
        use p256::elliptic_curve::rand_core::OsRng;
        *SigningKey::random(&mut OsRng).verifying_key()
    }

    fn req_with_workspace<T>(message: T, workspace: &str) -> Request<T> {
        let mut req = Request::new(message);
        req.extensions_mut()
            .insert(VerifiedWorkspace(workspace.to_string()));
        req
    }

    #[tokio::test]
    async fn get_turn_state_rejects_empty_conversation_id() {
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            GetTurnStateRequest {
                conversation_id: String::new(),
            },
            "ws",
        );
        let err = service.get_turn_state(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn get_turn_state_returns_idle_for_unknown_conversation() {
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            GetTurnStateRequest {
                conversation_id: "ws.conv-1".into(),
            },
            "ws",
        );
        let resp = service.get_turn_state(req).await.unwrap().into_inner();
        assert_eq!(resp.state, TurnState::Idle as i32);
    }

    #[tokio::test]
    async fn get_turn_state_reflects_recorded_failed_with_reason() {
        let service = make_service_with(fixture_verifier());
        let (tx, _rx) = mpsc::channel(4);
        let id = service.state.mint_channel("ws".into(), None, tx).await;
        service
            .state
            .set_and_broadcast_turn_failed(&id, "ws", "ws.conv-7", "boom", "13")
            .await;
        let req = req_with_workspace(
            GetTurnStateRequest {
                conversation_id: "ws.conv-7".into(),
            },
            "ws",
        );
        let resp = service.get_turn_state(req).await.unwrap().into_inner();
        assert_eq!(resp.state, TurnState::Failed as i32);
        assert_eq!(resp.reason, "boom");
        assert_eq!(resp.code, "13");
    }

    #[tokio::test]
    async fn get_turn_state_does_not_leak_across_workspaces() {
        // A turn recorded under "alpha" must be invisible to a client
        // verified as "beta", even given the same conversation_id.
        let service = make_service_with(fixture_verifier());
        let (tx, _rx) = mpsc::channel(4);
        let id = service.state.mint_channel("alpha".into(), None, tx).await;
        service
            .state
            .set_and_broadcast_turn_state(&id, "alpha", "conv", TurnState::Working)
            .await;
        let req = req_with_workspace(
            GetTurnStateRequest {
                conversation_id: "conv".into(),
            },
            "beta",
        );
        let resp = service.get_turn_state(req).await.unwrap().into_inner();
        assert_eq!(resp.state, TurnState::Idle as i32);
    }

    #[tokio::test]
    async fn get_turn_state_rejects_missing_extension() {
        let service = make_service_with(fixture_verifier());
        // No VerifiedWorkspace extension stamped.
        let req = Request::new(GetTurnStateRequest {
            conversation_id: "ws.conv".into(),
        });
        let err = service.get_turn_state(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn channel_ingest_rejects_empty_channel_id() {
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: String::new(),
                user_message: None,
                client_response: None,
                supported_methods: vec![],
                conversation_id: String::new(),
            },
            "ws",
        );
        let err = service.channel_ingest(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn channel_ingest_rejects_unregistered_channel() {
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: "never-minted".into(),
                user_message: Some(UserMessage {
                    content: vec![],
                    sender: "u".into(),
                    reply_channel: None,
                    conversation_id: String::new(),
                }),
                client_response: None,
                supported_methods: vec![],
                conversation_id: String::new(),
            },
            "ws",
        );
        let err = service.channel_ingest(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn channel_ingest_rejects_cross_workspace_channel() {
        let service = make_service_with(fixture_verifier());
        let (tx, _rx) = mpsc::channel(4);
        // Channel bound to "alpha".
        let id = service.state.mint_channel("alpha".into(), None, tx).await;
        // Caller verified as "beta".
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: id,
                user_message: Some(UserMessage {
                    content: vec![],
                    sender: "u".into(),
                    reply_channel: None,
                    conversation_id: String::new(),
                }),
                client_response: None,
                supported_methods: vec![],
                conversation_id: String::new(),
            },
            "beta",
        );
        let err = tokio::time::timeout(Duration::from_secs(1), service.channel_ingest(req))
            .await
            .expect("cross-workspace channel_ingest must reject without dialing hangar")
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn channel_ingest_rejects_both_payloads_set() {
        let service = make_service_with(fixture_verifier());
        let (tx, _rx) = mpsc::channel(4);
        let id = service.state.mint_channel("ws".into(), None, tx).await;
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: id,
                user_message: Some(UserMessage {
                    content: vec![],
                    sender: "u".into(),
                    reply_channel: None,
                    conversation_id: String::new(),
                }),
                client_response: Some(proto_common::ClientResponse {
                    request_id: "r".into(),
                    result_json: "{}".into(),
                    error: None,
                }),
                supported_methods: vec![],
                conversation_id: String::new(),
            },
            "ws",
        );
        let err = service.channel_ingest(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn channel_ingest_client_response_delivers_to_awaiter_and_acks_empty_conversation() {
        let service = make_service_with(fixture_verifier());
        let (tx, _rx) = mpsc::channel(4);
        let id = service.state.mint_channel("ws".into(), None, tx).await;
        // A client_response for an unknown request is benign — handler must
        // still ack with an empty conversation_id (no conversation routed).
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: id.clone(),
                user_message: None,
                client_response: Some(proto_common::ClientResponse {
                    request_id: "req-unknown".into(),
                    result_json: "{\"ok\":true}".into(),
                    error: None,
                }),
                supported_methods: vec![],
                conversation_id: String::new(),
            },
            "ws",
        );
        let ack = service.channel_ingest(req).await.unwrap().into_inner();
        assert_eq!(ack.channel_id, id);
        assert!(
            ack.conversation_id.is_empty(),
            "client_response path acks an empty conversation_id"
        );
    }

    #[tokio::test]
    async fn channel_ingest_advertised_supported_methods_become_usable() {
        let service = make_service_with(fixture_verifier());
        let (tx, _rx) = mpsc::channel(4);
        let id = service.state.mint_channel("ws".into(), None, tx).await;
        // A client_response payload exercises the supported_methods update
        // without dialing hangar. RevealPath is advertised here.
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: id.clone(),
                user_message: None,
                client_response: Some(proto_common::ClientResponse {
                    request_id: "req-unknown".into(),
                    result_json: "{}".into(),
                    error: None,
                }),
                supported_methods: vec!["RevealPath".into()],
                conversation_id: String::new(),
            },
            "ws",
        );
        service.channel_ingest(req).await.unwrap();
        // The advertised method is now dispatchable; otherwise
        // send_server_notification rejects with UnsupportedMethod.
        service
            .state
            .send_server_notification(&id, "RevealPath", "{}".into())
            .await
            .expect("advertised method must be usable");
    }

    #[tokio::test]
    async fn channel_ingest_empty_supported_methods_does_not_clobber() {
        let service = make_service_with(fixture_verifier());
        let (tx, _rx) = mpsc::channel(4);
        let id = service.state.mint_channel("ws".into(), None, tx).await;
        // Seed a prior advertised set.
        service
            .state
            .update_supported_methods(&id, vec!["RevealPath".into()])
            .await;
        // An ingest carrying NO supported_methods must not wipe the set.
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: id.clone(),
                user_message: None,
                client_response: Some(proto_common::ClientResponse {
                    request_id: "req-unknown".into(),
                    result_json: "{}".into(),
                    error: None,
                }),
                supported_methods: vec![],
                conversation_id: String::new(),
            },
            "ws",
        );
        service.channel_ingest(req).await.unwrap();
        service
            .state
            .send_server_notification(&id, "RevealPath", "{}".into())
            .await
            .expect("empty supported_methods must not clobber prior set");
    }

    #[tokio::test]
    async fn channel_ingest_client_response_rejects_neither_result_nor_error() {
        let service = make_service_with(fixture_verifier());
        let (tx, _rx) = mpsc::channel(4);
        let id = service.state.mint_channel("ws".into(), None, tx).await;
        // ClientResponse with empty result_json AND no error → neither set.
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: id,
                user_message: None,
                client_response: Some(proto_common::ClientResponse {
                    request_id: "r".into(),
                    result_json: String::new(),
                    error: None,
                }),
                supported_methods: vec![],
                conversation_id: String::new(),
            },
            "ws",
        );
        let err = service.channel_ingest(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn channel_receive_emits_ack_then_idle_replay() {
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            ChannelReceiveRequest {
                adapter_hint: Some("cli:test".into()),
            },
            "ws",
        );
        let resp = service.channel_receive(req).await.unwrap();
        let mut stream = resp.into_inner();
        let first = stream.next().await.unwrap().unwrap();
        let channel_id = match first.command {
            Some(channel_outbound::Command::Ack(a)) => a.channel_id,
            other => panic!("first frame must be Ack, got {other:?}"),
        };
        assert!(!channel_id.is_empty());
        let second = stream.next().await.unwrap().unwrap();
        match second.command {
            Some(channel_outbound::Command::TurnState(e)) => {
                assert_eq!(e.state, TurnState::Idle as i32);
                assert!(
                    e.conversation_id.is_empty(),
                    "replay frame carries empty conversation_id"
                );
            }
            other => panic!("second frame must be a TurnState replay, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_conversation_name_rejects_over_max_chars() {
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            SetConversationNameRequest {
                conversation_id: "ws.conv".into(),
                name: "x".repeat(MAX_CONVERSATION_NAME_CHARS + 1),
            },
            "ws",
        );
        let err = tokio::time::timeout(Duration::from_secs(1), service.set_conversation_name(req))
            .await
            .expect("over-limit name must be rejected without dialing hangar")
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn set_conversation_name_rejects_empty_id() {
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            SetConversationNameRequest {
                conversation_id: String::new(),
                name: "fine".into(),
            },
            "ws",
        );
        let err = service.set_conversation_name(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn delete_conversation_rejects_empty_id() {
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            DeleteConversationRequest {
                conversation_id: String::new(),
            },
            "ws",
        );
        let err = service.delete_conversation(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn get_conversation_history_rejects_empty_id() {
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            GetConversationHistoryRequest {
                conversation_id: String::new(),
                limit: None,
            },
            "ws",
        );
        let err = service.get_conversation_history(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn list_conversations_rejects_body_workspace_conflict() {
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            ListConversationsRequest {
                workspace: "beta".into(),
            },
            "alpha",
        );
        let err = tokio::time::timeout(Duration::from_secs(1), service.list_conversations(req))
            .await
            .expect("workspace conflict must be rejected without dialing hangar")
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn list_workspaces_returns_workspaces_for_verified_client() {
        let verifier = fixture_verifier();
        verifier.registrations().write().await.insert(
            "kid-1".into(),
            ClientRegistration {
                verifying_key: fresh_vk(),
                workspaces: vec!["alpha".into(), "beta".into()],
            },
        );
        let service = make_service_with(verifier);
        let mut req = Request::new(ListWorkspacesRequest {});
        req.extensions_mut().insert(VerifiedClient("kid-1".into()));
        let resp = service.list_workspaces(req).await.unwrap().into_inner();
        assert_eq!(
            resp.workspaces,
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[tokio::test]
    async fn list_workspaces_rejects_unknown_kid() {
        let service = make_service_with(fixture_verifier());
        let mut req = Request::new(ListWorkspacesRequest {});
        req.extensions_mut().insert(VerifiedClient("ghost".into()));
        let err = service.list_workspaces(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn list_workspaces_rejects_missing_extension() {
        let service = make_service_with(fixture_verifier());
        let req = Request::new(ListWorkspacesRequest {});
        let err = service.list_workspaces(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn redeem_enrollment_without_kube_client_fails_precondition() {
        // verify_enrollment_code must pass first, then the missing kube
        // client trips FailedPrecondition. Build a code with the state's
        // own signing key so verification succeeds.
        let state = Arc::new(GatewayState::new(
            fixture_verifier(),
            fixture_signing_key(),
            None,
            "default".into(),
        ));
        let code = crate::enrollment::sign_enrollment_code(
            state.signing_key(),
            "ws",
            "device",
            "code-1",
            3600,
        );
        let service = GatewayService::new(state);
        let req = Request::new(RedeemEnrollmentRequest {
            enrollment_code: code,
            public_key: vec![],
        });
        let err = service.redeem_enrollment(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn redeem_enrollment_rejects_bad_code() {
        let service = make_service_with(fixture_verifier());
        let req = Request::new(RedeemEnrollmentRequest {
            enrollment_code: "not-a-jwt".into(),
            public_key: vec![],
        });
        let err = service.redeem_enrollment(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn workspace_claim_conflicts_empty_body_is_accepted() {
        assert!(!workspace_claim_conflicts("", "alpha"));
    }

    #[test]
    fn workspace_claim_conflicts_matching_body_is_accepted() {
        assert!(!workspace_claim_conflicts("alpha", "alpha"));
    }

    #[test]
    fn workspace_claim_conflicts_mismatched_body_conflicts() {
        assert!(workspace_claim_conflicts("beta", "alpha"));
    }

    #[test]
    fn require_conversation_name_within_limit_enforces_boundary() {
        let cases = [
            (1_usize, true),
            (MAX_CONVERSATION_NAME_CHARS, true),
            (MAX_CONVERSATION_NAME_CHARS + 1, false),
        ];
        for (char_count, expect_ok) in cases {
            let name = "x".repeat(char_count);
            let result = require_conversation_name_within_limit(&name);
            assert_eq!(
                result.is_ok(),
                expect_ok,
                "{char_count} chars: expected ok={expect_ok}"
            );
            if let Err(status) = result {
                assert_eq!(status.code(), tonic::Code::InvalidArgument);
            }
        }
    }

    #[test]
    fn warn_on_undelivered_warns_only_when_not_delivered() {
        for (delivered, expect_warn) in [(true, false), (false, true)] {
            assert_eq!(
                warn_on_undelivered(delivered),
                expect_warn,
                "delivered={delivered} must warn={expect_warn}"
            );
        }
    }

    // CancelTurn travels back through the gateway to the transponder as a pure
    // relay, applying the same guard-then-forward contract as its sibling
    // lifecycle RPCs (delete_conversation / get_turn_state): a CancelTurn with
    // no conversation_id is rejected at the gateway, never forwarded blind.
    #[tokio::test]
    async fn cancel_turn_rejects_empty_conversation_id() {
        // Materiality: drop the empty-id guard on the gateway's cancel_turn
        // forwarder -> an unkeyed CancelTurn is dialed at the transponder
        // instead of failing fast with InvalidArgument.
        let service = make_service_with(fixture_verifier());
        let req = req_with_workspace(
            proto_common::CancelTurnRequest {
                conversation_id: String::new(),
            },
            "ws",
        );
        let err = service.cancel_turn(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }
}
