//! `RelayGateway` service — the signature-verified external surface.
//!
//! Two flavors of handler:
//!
//! - **Local-state handlers** own the live channel/subscriber/turn-state
//!   surface (channel_ingest, channel_receive, get_turn_state,
//!   redeem_code, list_workspaces).
//! - **Forwarding handlers** (mint_conversation, list_conversations,
//!   delete_conversation, set_conversation_name, get_conversation_history,
//!   watch_tools, call_tool) carry no local state: the durable conversation
//!   log lives in the per-workspace harness, so they pick the caller's
//!   harness from the pool and pass the call through. Authorization is
//!   established here: the verified grant row is resolved against the live
//!   grants table on every request, and a conversation is reachable only by
//!   the row that minted it. The forwarded SA token authenticates relay to
//!   the harness.

use std::sync::Arc;

use futures::StreamExt;
use proto_common::{
    channel_outbound, AwaitToolResultRequest, CallToolRequest, CancelToolRequest,
    CancelToolResponse, CancelTurnRequest, CancelTurnResponse, ChannelAck, ChannelIngestAck,
    ChannelIngestRequest, ChannelOutbound, ChannelReceiveRequest, DeleteConversationRequest,
    DeleteConversationResponse, DispatchToolResponse, GetConversationHistoryRequest,
    GetConversationHistoryResponse, GetTurnStateRequest, ListConversationsRequest,
    ListConversationsResponse, ListGrantsRequest, ListGrantsResponse, ListWorkspacesRequest,
    ListWorkspacesResponse, MintConversationRequest, MintConversationResponse, RedeemCodeRequest,
    RedeemCodeResponse, SetConversationNameRequest, SetConversationNameResponse, ToolListUpdate,
    ToolResultFrame, ToolsetGrants, TurnState, TurnStateEvent, UserMessage, WatchToolsRequest,
};
use relay_proto::relay_gateway_server::RelayGateway;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::grants::GrantRow;
use crate::state::GatewayState;

/// Seconds to keep the channel's outbound side open after the client
/// half-closes. Just under the 60s default gRPC client deadline.
const CHANNEL_DRAIN_SECS: u64 = 55;

/// Server-side clamp ceiling for `SetConversationName`. Enforced before
/// the forward so a hostile name never reaches the harness.
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

/// Whether a request bound to `verified_row` may touch a conversation the
/// relay has cached under `cached_owner`.
///
/// The cache is a cache: a miss is not permission. It sends the relay to the
/// harness, which holds the durable owner stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAccess {
    Allow,
    Deny,
    ResolveWithHarness,
}

/// Pure conversation-ownership decision. Takes no workspace: two rows in
/// one workspace must not reach each other's conversations, so a workspace
/// comparison is exactly the thing that must not appear here.
pub fn conversation_access(cached_owner: Option<&str>, verified_row: &str) -> RowAccess {
    match cached_owner {
        Some(owner) if owner == verified_row => RowAccess::Allow,
        Some(_) => RowAccess::Deny,
        None => RowAccess::ResolveWithHarness,
    }
}

#[allow(clippy::result_large_err)]
fn denied_conversation() -> Status {
    Status::permission_denied("conversation belongs to a different grant row")
}

impl GatewayService {
    pub fn new(state: Arc<GatewayState>) -> Self {
        Self { state }
    }

    /// Read the grant row the signature middleware verified and stamped on
    /// the request extensions. The listener trusts this entirely — the
    /// `SignatureLayer` ran first and populated it.
    #[allow(clippy::result_large_err)]
    fn verified_row<T>(request: &Request<T>) -> Result<String, Status> {
        request
            .extensions()
            .get::<crate::signature_layer::VerifiedRow>()
            .map(|r| r.0.clone())
            .ok_or_else(|| {
                Status::permission_denied(
                    "missing verified row extension; middleware must populate it",
                )
            })
    }

    /// The verified row, checked against the live grants table. A row the
    /// operator has removed resolves to nothing, which is how revocation
    /// takes effect on the next request without a pod restart.
    #[allow(clippy::result_large_err)]
    async fn authorized_row<T>(&self, request: &Request<T>) -> Result<(String, GrantRow), Status> {
        let row_key = Self::verified_row(request)?;
        let grants = self.state.grants();
        let table = grants.read().await;
        match table.get(&row_key) {
            Some(row) => Ok((row_key, row.clone())),
            None => Err(Status::permission_denied(
                "grant row is not in the live grants table",
            )),
        }
    }

    /// Refuse a request naming a conversation minted under another row,
    /// including when both rows name the same workspace. An empty id names
    /// no conversation and is passed through untouched.
    #[allow(clippy::result_large_err)]
    async fn require_conversation_row(
        &self,
        workspace: &str,
        row: &str,
        conversation_id: &str,
    ) -> Result<(), Status> {
        if conversation_id.is_empty() {
            return Ok(());
        }
        let cached = self.state.conversation_owner(conversation_id).await;
        match conversation_access(cached.as_deref(), row) {
            RowAccess::Allow => Ok(()),
            RowAccess::Deny => Err(denied_conversation()),
            RowAccess::ResolveWithHarness => {
                self.resolve_conversation_owner(workspace, row, conversation_id)
                    .await
            }
        }
    }

    /// Ask the harness — the holder of the durable owner stamp — whether
    /// this row minted this conversation, and cache the answer. Denies when
    /// the harness does not list it for this row.
    #[allow(clippy::result_large_err)]
    async fn resolve_conversation_owner(
        &self,
        workspace: &str,
        row: &str,
        conversation_id: &str,
    ) -> Result<(), Status> {
        let mut client = self
            .state
            .harness_clients()
            .get(workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        let listed = client
            .as_mut()
            .list_conversations(ListConversationsRequest {
                workspace: workspace.to_string(),
                owner: row.to_string(),
            })
            .await?
            .into_inner();
        if listed
            .conversations
            .iter()
            .any(|c| c.conversation_id == conversation_id)
        {
            self.state
                .record_conversation_owner(conversation_id, row)
                .await;
            Ok(())
        } else {
            Err(denied_conversation())
        }
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
impl RelayGateway for GatewayService {
    async fn redeem_code(
        &self,
        request: Request<RedeemCodeRequest>,
    ) -> Result<Response<RedeemCodeResponse>, Status> {
        // Unauthenticated by design — the operator-written code IS the
        // authentication artifact, and the identity of the row it names.
        // The relay mints nothing here: no code, no identity, no row.
        let req = request.into_inner();

        let (row_key, row) = {
            let grants = self.state.grants();
            let table = grants.read().await;
            table
                .find_by_code(&req.code)
                .map(|(key, row)| (key.to_string(), row.clone()))
                .ok_or_else(|| Status::permission_denied("presented code matches no grant row"))?
        };

        // One-shot: the row is spent once a device key is registered
        // against it. Revoke-and-re-invite is delete the row, write a new
        // one with a fresh code.
        let registrations = self.state.client_verifier().registrations();
        {
            // Check and claim in one write-lock scope, so a concurrent
            // redemption of the same code is refused here rather than
            // racing through the window a Kubernetes roundtrip would open.
            // Nothing awaits while the lock is held.
            //
            // The claim doubles as the synchronous install: this device's
            // immediate signed follow-up (ListWorkspaces) verifies without
            // waiting for a restart-time rebuild.
            let mut map = registrations.write().await;
            if map.contains_key(&row_key) {
                return Err(Status::permission_denied(
                    "grant row already has a registered device key",
                ));
            }

            let verifying_key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&req.public_key)
                .map_err(|_| {
                    Status::invalid_argument("public_key is not a valid P-256 SEC1 point")
                })?;

            map.insert(
                row_key.clone(),
                shared::client_signature::ClientRegistration {
                    verifying_key,
                    workspace: row.workspace.clone(),
                },
            );
        }

        let outcome = match self.state.kube_client() {
            None => Err(Status::failed_precondition("controller has no kube client")),
            Some(kube_client) => {
                crate::registered_keys::register_key(
                    &kube_client.clone(),
                    self.state.namespace(),
                    &row_key,
                    &req.public_key,
                )
                .await
            }
        };
        if let Err(status) = outcome {
            // Roll the claim back so the row stays redeemable.
            registrations.write().await.remove(&row_key);
            return Err(status);
        }

        tracing::info!(row = %row_key, workspace = %row.workspace, "grant row redeemed");

        Ok(Response::new(RedeemCodeResponse {
            client_name: row_key,
            enrolled_at: chrono::Utc::now().timestamp(),
        }))
    }

    async fn list_workspaces(
        &self,
        request: Request<ListWorkspacesRequest>,
    ) -> Result<Response<ListWorkspacesResponse>, Status> {
        // A grant row names exactly one workspace, and the answer comes
        // from the live table — not from the registered-key store, which
        // would keep answering after the operator removed the row.
        let (_row_key, row) = self.authorized_row(&request).await?;
        Ok(Response::new(ListWorkspacesResponse {
            workspaces: vec![row.workspace],
        }))
    }

    async fn list_grants(
        &self,
        request: Request<ListGrantsRequest>,
    ) -> Result<Response<ListGrantsResponse>, Status> {
        // The menu is answered for the verified row's workspace, never the
        // body's; a non-empty body workspace must agree with the claim.
        let (_row_key, row) = self.authorized_row(&request).await?;
        let req = request.into_inner();
        if workspace_claim_conflicts(&req.workspace, &row.workspace) {
            return Err(Status::permission_denied(
                "workspace claim does not match request body",
            ));
        }
        let toolsets = self
            .state
            .credentials()
            .for_workspace(&row.workspace)
            .into_iter()
            .map(|(toolset, grants)| ToolsetGrants { toolset, grants })
            .collect();
        Ok(Response::new(ListGrantsResponse { toolsets }))
    }

    async fn mint_conversation(
        &self,
        request: Request<MintConversationRequest>,
    ) -> Result<Response<MintConversationResponse>, Status> {
        // The verified row picks the harness that owns this caller's
        // conversation log, and stamps the conversation as the row's.
        let (row_key, row) = self.authorized_row(&request).await?;
        let mut client = self
            .state
            .harness_clients()
            .get(&row.workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        let resp = client
            .as_mut()
            .mint_conversation(MintConversationRequest {
                owner: row_key.clone(),
            })
            .await?;
        self.state
            .record_conversation_owner(&resp.get_ref().conversation_id, &row_key)
            .await;
        Ok(resp)
    }

    async fn list_conversations(
        &self,
        request: Request<ListConversationsRequest>,
    ) -> Result<Response<ListConversationsResponse>, Status> {
        let (row_key, row) = self.authorized_row(&request).await?;
        let req = request.into_inner();
        if workspace_claim_conflicts(&req.workspace, &row.workspace) {
            return Err(Status::permission_denied(
                "workspace claim does not match request body",
            ));
        }
        let mut client = self
            .state
            .harness_clients()
            .get(&row.workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        client
            .as_mut()
            .list_conversations(ListConversationsRequest {
                workspace: req.workspace,
                owner: row_key,
            })
            .await
    }

    async fn delete_conversation(
        &self,
        request: Request<DeleteConversationRequest>,
    ) -> Result<Response<DeleteConversationResponse>, Status> {
        let (row_key, row) = self.authorized_row(&request).await?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "DeleteConversationRequest.conversation_id required",
            ));
        }
        self.require_conversation_row(&row.workspace, &row_key, &req.conversation_id)
            .await?;
        let mut client = self
            .state
            .harness_clients()
            .get(&row.workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        client.as_mut().delete_conversation(req).await
    }

    async fn cancel_turn(
        &self,
        request: Request<CancelTurnRequest>,
    ) -> Result<Response<CancelTurnResponse>, Status> {
        let (row_key, row) = self.authorized_row(&request).await?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "CancelTurnRequest.conversation_id required",
            ));
        }
        self.require_conversation_row(&row.workspace, &row_key, &req.conversation_id)
            .await?;
        let mut client = self
            .state
            .harness_clients()
            .get(&row.workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        client.as_mut().cancel_turn(req).await
    }

    async fn set_conversation_name(
        &self,
        request: Request<SetConversationNameRequest>,
    ) -> Result<Response<SetConversationNameResponse>, Status> {
        let (row_key, row) = self.authorized_row(&request).await?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "SetConversationNameRequest.conversation_id required",
            ));
        }
        require_conversation_name_within_limit(&req.name)?;
        self.require_conversation_row(&row.workspace, &row_key, &req.conversation_id)
            .await?;
        let mut client = self
            .state
            .harness_clients()
            .get(&row.workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        client.as_mut().set_conversation_name(req).await
    }

    async fn get_conversation_history(
        &self,
        request: Request<GetConversationHistoryRequest>,
    ) -> Result<Response<GetConversationHistoryResponse>, Status> {
        let (row_key, row) = self.authorized_row(&request).await?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument("conversation_id required"));
        }
        let mut client = self
            .state
            .harness_clients()
            .get(&row.workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        client
            .as_mut()
            .get_conversation_history(GetConversationHistoryRequest {
                conversation_id: req.conversation_id,
                limit: req.limit,
                owner: row_key,
            })
            .await
    }

    async fn get_turn_state(
        &self,
        request: Request<GetTurnStateRequest>,
    ) -> Result<Response<TurnStateEvent>, Status> {
        // Read-only poll of the gateway-owned per-conversation turn phase,
        // keyed by the caller's verified grant row so it can only read its
        // own — two rows in one workspace see different maps. Never
        // dispatches to the LLM.
        let (row_key, _row) = self.authorized_row(&request).await?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument("conversation_id required"));
        }
        // Unknown / never-active conversation → IDLE. Absence of a record
        // is not an error — it is exactly the "input enabled, nothing in
        // flight" state the client wants.
        let event = match self
            .state
            .turn_state_record(&row_key, &req.conversation_id)
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
        // The grant row is derived from the caller's signature — NEVER from
        // the request body. The caller echoes the server-minted channel_id
        // received earlier from ChannelReceive's first ChannelAck frame; we
        // verify it belongs to the caller's verified row before routing.
        let (row_key, row) = self.authorized_row(&request).await?;
        let workspace = row.workspace.clone();
        let req = request.into_inner();
        if req.channel_id.is_empty() {
            return Err(Status::invalid_argument(
                "ChannelIngestRequest.channel_id required",
            ));
        }
        match self.state.channel_row(&req.channel_id).await {
            Some(bound) if bound == row_key => {}
            Some(_) => {
                return Err(Status::permission_denied(
                    "ChannelIngestRequest.channel_id is bound to a different grant row",
                ));
            }
            None => {
                return Err(Status::not_found(
                    "ChannelIngestRequest.channel_id is not registered (call ChannelReceive first)",
                ));
            }
        }
        self.require_conversation_row(&workspace, &row_key, &req.conversation_id)
            .await?;

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
                // harness; non-empty (opaque UUID) → continue it.
                let conversation_id = if req.conversation_id.is_empty() {
                    let mut client = self
                        .state
                        .harness_clients()
                        .get(&workspace)
                        .await
                        .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
                    let minted = client
                        .as_mut()
                        .mint_conversation(MintConversationRequest {
                            owner: row_key.clone(),
                        })
                        .await?
                        .into_inner()
                        .conversation_id;
                    self.state
                        .record_conversation_owner(&minted, &row_key)
                        .await;
                    minted
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
                            // The client's grant selections ride through
                            // untouched; the controller is the closed-set
                            // authority, not the relay.
                            grants: user_message.grants,
                        },
                    )
                    .await;
                // The user message has been accepted and routed; the
                // harness will pick it up momentarily. Move the channel
                // to WORKING so the client renders an active indicator until
                // the assistant message (or a TurnError) lands.
                self.state
                    .set_and_broadcast_turn_state(
                        &req.channel_id,
                        &row_key,
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
        let (row_key, row) = self.authorized_row(&request).await?;
        let req = request.into_inner();

        let (tx, rx) = mpsc::channel(16);
        // Mint the server-side channel_id, bind it to the verified grant
        // row and that row's workspace, and stash the adapter_hint for log
        // emission.
        let channel_id = self
            .state
            .mint_channel(
                row_key.clone(),
                row.workspace.clone(),
                req.adapter_hint.clone(),
                tx.clone(),
            )
            .await;
        tracing::info!(
            channel_id = %channel_id,
            row = %row_key,
            workspace = %row.workspace,
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
        let (_row_key, row) = self.authorized_row(&request).await?;
        let mut client = self
            .state
            .harness_clients()
            .get(&row.workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        let upstream = client
            .as_mut()
            .watch_tools(WatchToolsRequest {})
            .await?
            .into_inner();
        // 1:1 pass-through. tonic Streaming<T> is already a
        // futures::Stream<Item = Result<T, Status>>.
        Ok(Response::new(Box::pin(upstream)))
    }

    async fn dispatch_tool(
        &self,
        request: Request<CallToolRequest>,
    ) -> Result<Response<DispatchToolResponse>, Status> {
        let (row_key, row) = self.authorized_row(&request).await?;
        let req = request.into_inner();
        // conversation_id is optional here; when one is named it must
        // belong to the calling row. No per-key, per-row, or per-tool
        // restriction beyond that and the workspace boundary.
        self.require_conversation_row(&row.workspace, &row_key, &req.conversation_id)
            .await?;
        let mut client = self
            .state
            .harness_clients()
            .get(&row.workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        client.as_mut().dispatch_tool(req).await
    }

    type AwaitToolResultStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<ToolResultFrame, Status>> + Send>>;

    async fn await_tool_result(
        &self,
        request: Request<AwaitToolResultRequest>,
    ) -> Result<Response<Self::AwaitToolResultStream>, Status> {
        let (row_key, row) = self.authorized_row(&request).await?;
        let req = request.into_inner();
        if req.call_id.is_empty() {
            return Err(Status::invalid_argument(
                "AwaitToolResultRequest.call_id required",
            ));
        }
        self.require_conversation_row(&row.workspace, &row_key, &req.conversation_id)
            .await?;
        let mut client = self
            .state
            .harness_clients()
            .get(&row.workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        let upstream = client.as_mut().await_tool_result(req).await?.into_inner();
        // 1:1 pass-through. tonic Streaming<T> is already a
        // futures::Stream<Item = Result<T, Status>>.
        Ok(Response::new(Box::pin(upstream)))
    }

    async fn cancel_tool(
        &self,
        request: Request<CancelToolRequest>,
    ) -> Result<Response<CancelToolResponse>, Status> {
        let (_row_key, row) = self.authorized_row(&request).await?;
        let req = request.into_inner();
        if req.call_id.is_empty() {
            return Err(Status::invalid_argument(
                "CancelToolRequest.call_id required",
            ));
        }
        let mut client = self
            .state
            .harness_clients()
            .get(&row.workspace)
            .await
            .map_err(|e| Status::unavailable(format!("harness unavailable: {e}")))?;
        client.as_mut().cancel_tool(req).await
    }
}

/// Reject a conversation name longer than `MAX_CONVERSATION_NAME_CHARS`
/// Unicode scalar values, before the harness forward.
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
    use crate::grants::apply_delivery;
    use crate::signature_layer::VerifiedRow;
    use k8s_openapi::api::core::v1::ConfigMap;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use shared::client_signature::ClientSignatureVerifier;
    use std::collections::BTreeMap;
    use std::time::Duration;

    /// One grant row per test caller. `row-<ws>` names workspace `<ws>`, so
    /// a fixture's row and its workspace stay legible side by side.
    fn fixture_grants() -> crate::grants::GrantsTable {
        let mut data = BTreeMap::new();
        for ws in ["ws", "alpha", "beta", "hello-world"] {
            data.insert(
                format!("row-{ws}"),
                format!("channel: app\nidentity: code-{ws}\nworkspace: {ws}\n"),
            );
        }
        let cm = ConfigMap {
            metadata: ObjectMeta {
                name: Some("grants".into()),
                namespace: Some("default".into()),
                ..Default::default()
            },
            data: Some(data),
            ..Default::default()
        };
        let (table, errors) = apply_delivery(&cm);
        assert!(errors.is_empty());
        table
    }

    fn fixture_verifier() -> Arc<ClientSignatureVerifier> {
        Arc::new(ClientSignatureVerifier::new(Duration::from_secs(300)))
    }

    async fn make_service_with(verifier: Arc<ClientSignatureVerifier>) -> GatewayService {
        let state = Arc::new(GatewayState::new(verifier, None, "default".into()));
        *state.grants().write().await = fixture_grants();
        GatewayService::new(state)
    }

    /// Like `make_service_with`, but the harness pool dials a closed
    /// local port, so a forwarded call refuses immediately instead of
    /// resolving cluster DNS.
    async fn make_service_dialing_closed_port(
        verifier: Arc<ClientSignatureVerifier>,
    ) -> GatewayService {
        let pool = crate::harness_client::HarnessClientPool::from_service_template(
            "http://127.0.0.1:1".into(),
        );
        let state = Arc::new(GatewayState::new_with_harness_pool(
            verifier,
            None,
            "default".into(),
            pool,
        ));
        *state.grants().write().await = fixture_grants();
        GatewayService::new(state)
    }

    /// In-process harness stand-in. `list_conversations` answers from
    /// `conversations`; the three tool forwarders answer with fixed values
    /// and record the request they were handed, so a test can assert the
    /// relay passed it through untouched. Every other method is
    /// unimplemented, so a test that reaches one has escaped the check it
    /// meant to exercise and fails on the wrong status.
    #[derive(Default)]
    struct MockHarness {
        conversations: Vec<String>,
        seen_dispatch: Arc<std::sync::Mutex<Option<CallToolRequest>>>,
        seen_await: Arc<std::sync::Mutex<Option<AwaitToolResultRequest>>>,
        seen_cancel: Arc<std::sync::Mutex<Option<CancelToolRequest>>>,
    }

    #[tonic::async_trait]
    impl harness_proto::harness_control_server::HarnessControl for MockHarness {
        type WatchToolsStream = futures::stream::Empty<Result<ToolListUpdate, Status>>;
        type AwaitToolResultStream =
            futures::stream::Iter<std::vec::IntoIter<Result<ToolResultFrame, Status>>>;

        async fn list_conversations(
            &self,
            _request: Request<ListConversationsRequest>,
        ) -> Result<Response<ListConversationsResponse>, Status> {
            Ok(Response::new(ListConversationsResponse {
                conversations: self
                    .conversations
                    .iter()
                    .map(|id| proto_common::ConversationSummary {
                        conversation_id: id.clone(),
                        ..Default::default()
                    })
                    .collect(),
            }))
        }

        async fn watch_tools(
            &self,
            _request: Request<WatchToolsRequest>,
        ) -> Result<Response<Self::WatchToolsStream>, Status> {
            Err(Status::unimplemented("mock harness"))
        }

        async fn dispatch_tool(
            &self,
            request: Request<CallToolRequest>,
        ) -> Result<Response<DispatchToolResponse>, Status> {
            *self.seen_dispatch.lock().unwrap() = Some(request.into_inner());
            Ok(Response::new(DispatchToolResponse {
                call_id: "call-42".into(),
            }))
        }

        async fn await_tool_result(
            &self,
            request: Request<AwaitToolResultRequest>,
        ) -> Result<Response<Self::AwaitToolResultStream>, Status> {
            *self.seen_await.lock().unwrap() = Some(request.into_inner());
            Ok(Response::new(futures::stream::iter(vec![
                Ok(ToolResultFrame {
                    frame: Some(proto_common::tool_result_frame::Frame::Stdout("out".into())),
                }),
                Ok(ToolResultFrame {
                    frame: Some(proto_common::tool_result_frame::Frame::Complete(
                        proto_common::ToolComplete::default(),
                    )),
                }),
            ])))
        }

        async fn cancel_tool(
            &self,
            request: Request<CancelToolRequest>,
        ) -> Result<Response<CancelToolResponse>, Status> {
            *self.seen_cancel.lock().unwrap() = Some(request.into_inner());
            Ok(Response::new(CancelToolResponse { cancelled: true }))
        }

        async fn mint_conversation(
            &self,
            _request: Request<MintConversationRequest>,
        ) -> Result<Response<MintConversationResponse>, Status> {
            Err(Status::unimplemented("mock harness"))
        }

        async fn delete_conversation(
            &self,
            _request: Request<DeleteConversationRequest>,
        ) -> Result<Response<DeleteConversationResponse>, Status> {
            Err(Status::unimplemented("mock harness"))
        }

        async fn set_conversation_name(
            &self,
            _request: Request<SetConversationNameRequest>,
        ) -> Result<Response<SetConversationNameResponse>, Status> {
            Err(Status::unimplemented("mock harness"))
        }

        async fn get_conversation_history(
            &self,
            _request: Request<GetConversationHistoryRequest>,
        ) -> Result<Response<GetConversationHistoryResponse>, Status> {
            Err(Status::unimplemented("mock harness"))
        }

        async fn cancel_turn(
            &self,
            _request: Request<CancelTurnRequest>,
        ) -> Result<Response<CancelTurnResponse>, Status> {
            Err(Status::unimplemented("mock harness"))
        }
    }

    /// Serve `mock` on an ephemeral loopback port and return its address.
    async fn spawn_mock_harness(mock: MockHarness) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(harness_proto::harness_control_server::HarnessControlServer::new(mock))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );
        addr
    }

    /// Like `make_service_dialing_closed_port`, but the pool reaches a live
    /// mock harness, so the resolve path runs end to end.
    async fn make_service_dialing(addr: std::net::SocketAddr) -> GatewayService {
        let pool = crate::harness_client::HarnessClientPool::from_service_template(format!(
            "http://{addr}"
        ));
        let state = Arc::new(GatewayState::new_with_harness_pool(
            fixture_verifier(),
            None,
            "default".into(),
            pool,
        ));
        *state.grants().write().await = fixture_grants();
        GatewayService::new(state)
    }

    /// Sign as the row that owns `workspace`, and pre-cache `conversation`
    /// under it so the ownership check resolves without a harness dial.
    fn req_with_workspace<T>(message: T, workspace: &str) -> Request<T> {
        let mut req = Request::new(message);
        req.extensions_mut()
            .insert(VerifiedRow(format!("row-{workspace}")));
        req
    }

    #[tokio::test]
    async fn get_turn_state_rejects_empty_conversation_id() {
        let service = make_service_with(fixture_verifier()).await;
        let req = req_with_workspace(
            GetTurnStateRequest {
                conversation_id: String::new(),
            },
            "ws",
        );
        let err = service.get_turn_state(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    // conversation_id is optional on dispatch: the gateway does NOT fast-reject an
    // empty id — a conversation-less app-run call (the browser pane before any chat
    // is selected) is forwarded to the harness, which owns the conversation
    // semantics. Here it reaches the dial and fails Unavailable (no harness
    // configured in the test), never InvalidArgument.
    //
    // Materiality: reinstate an empty-conversation_id fast-reject and the code is
    // InvalidArgument instead of Unavailable, so this reds.
    #[tokio::test(start_paused = true)]
    async fn dispatch_tool_forwards_empty_conversation_id() {
        let service = make_service_dialing_closed_port(fixture_verifier()).await;
        let req = req_with_workspace(
            CallToolRequest {
                name: "Bash".into(),
                input_json: "{}".into(),
                conversation_id: String::new(),
            },
            "ws",
        );
        let err = service.dispatch_tool(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn get_turn_state_returns_idle_for_unknown_conversation() {
        let service = make_service_with(fixture_verifier()).await;
        let req = req_with_workspace(
            GetTurnStateRequest {
                conversation_id: "ws.conv-1".into(),
            },
            "ws",
        );
        let resp = service.get_turn_state(req).await.unwrap().into_inner();
        assert_eq!(resp.state, TurnState::Idle as i32);
        // Kills gateway.rs None-arm conversation_id (343): the IDLE reply must
        // echo the polled conversation_id so the client can correlate it;
        // dropping the field defaults it to empty and the poll can't be matched.
        assert_eq!(resp.conversation_id, "ws.conv-1");
        // reason/code carry the IDLE contract (no phantom failure text). These
        // do NOT kill the field-delete mutants at 344/345: the explicit value
        // is `String::new()`, identical to the `..Default::default()` fallback,
        // so those two are equivalent mutants with no behavioral witness.
        assert!(resp.reason.is_empty());
        assert!(resp.code.is_empty());
    }

    #[tokio::test]
    async fn get_turn_state_reflects_recorded_failed_with_reason() {
        let service = make_service_with(fixture_verifier()).await;
        let (tx, _rx) = mpsc::channel(4);
        let id = service
            .state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        service
            .state
            .set_and_broadcast_turn_failed(&id, "row-ws", "ws.conv-7", "boom", "13")
            .await;
        let req = req_with_workspace(
            GetTurnStateRequest {
                conversation_id: "ws.conv-7".into(),
            },
            "ws",
        );
        let resp = service.get_turn_state(req).await.unwrap().into_inner();
        assert_eq!(resp.state, TurnState::Failed as i32);
        // Kills gateway.rs Some-arm conversation_id: the recorded reply must
        // echo the polled conversation_id so the client can correlate it.
        assert_eq!(resp.conversation_id, "ws.conv-7");
        assert_eq!(resp.reason, "boom");
        assert_eq!(resp.code, "13");
    }

    #[tokio::test]
    async fn get_turn_state_does_not_leak_across_rows() {
        // A turn recorded under row-alpha must be invisible to a client
        // verified as row-beta, even given the same conversation_id.
        let service = make_service_with(fixture_verifier()).await;
        let (tx, _rx) = mpsc::channel(4);
        let id = service
            .state
            .mint_channel("row-alpha".into(), "alpha".into(), None, tx)
            .await;
        service
            .state
            .set_and_broadcast_turn_state(&id, "row-alpha", "conv", TurnState::Working)
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
        let service = make_service_with(fixture_verifier()).await;
        // No VerifiedWorkspace extension stamped.
        let req = Request::new(GetTurnStateRequest {
            conversation_id: "ws.conv".into(),
        });
        let err = service.get_turn_state(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn channel_ingest_rejects_empty_channel_id() {
        let service = make_service_with(fixture_verifier()).await;
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
        let service = make_service_with(fixture_verifier()).await;
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: "never-minted".into(),
                user_message: Some(UserMessage {
                    grants: vec![],
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
        let service = make_service_with(fixture_verifier()).await;
        let (tx, _rx) = mpsc::channel(4);
        // Channel bound to "alpha".
        let id = service
            .state
            .mint_channel("row-alpha".into(), "alpha".into(), None, tx)
            .await;
        // Caller verified as "beta".
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: id,
                user_message: Some(UserMessage {
                    grants: vec![],
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
            .expect("cross-workspace channel_ingest must reject without dialing the harness")
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn channel_ingest_rejects_both_payloads_set() {
        let service = make_service_with(fixture_verifier()).await;
        let (tx, _rx) = mpsc::channel(4);
        let id = service
            .state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        let req = req_with_workspace(
            ChannelIngestRequest {
                channel_id: id,
                user_message: Some(UserMessage {
                    grants: vec![],
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
        let service = make_service_with(fixture_verifier()).await;
        let (tx, _rx) = mpsc::channel(4);
        let id = service
            .state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
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
        let service = make_service_with(fixture_verifier()).await;
        let (tx, _rx) = mpsc::channel(4);
        let id = service
            .state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        // A client_response payload exercises the supported_methods update
        // without dialing the harness. RevealPath is advertised here.
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
        let service = make_service_with(fixture_verifier()).await;
        let (tx, _rx) = mpsc::channel(4);
        let id = service
            .state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
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
        let service = make_service_with(fixture_verifier()).await;
        let (tx, _rx) = mpsc::channel(4);
        let id = service
            .state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
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
        let service = make_service_with(fixture_verifier()).await;
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
        let service = make_service_with(fixture_verifier()).await;
        let req = req_with_workspace(
            SetConversationNameRequest {
                conversation_id: "ws.conv".into(),
                name: "x".repeat(MAX_CONVERSATION_NAME_CHARS + 1),
            },
            "ws",
        );
        let err = tokio::time::timeout(Duration::from_secs(1), service.set_conversation_name(req))
            .await
            .expect("over-limit name must be rejected without dialing the harness")
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn set_conversation_name_rejects_empty_id() {
        let service = make_service_with(fixture_verifier()).await;
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
        let service = make_service_with(fixture_verifier()).await;
        let req = req_with_workspace(
            DeleteConversationRequest {
                conversation_id: String::new(),
            },
            "ws",
        );
        let err = service.delete_conversation(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    /// The row owns a conversation, just not this one. A cold cache must
    /// deny on the harness's answer rather than on the mere presence of a
    /// listing, so an equality slip that turns the membership test into a
    /// non-membership test hands the row its neighbour's conversation.
    #[tokio::test]
    async fn cold_cache_resolve_denies_a_conversation_the_harness_lists_for_other_ids() {
        let addr = spawn_mock_harness(MockHarness {
            conversations: vec!["ws.other".into()],
            ..Default::default()
        })
        .await;
        let service = make_service_dialing(addr).await;
        let req = req_with_workspace(
            DeleteConversationRequest {
                conversation_id: "ws.conv-1".into(),
            },
            "ws",
        );
        let err = service.delete_conversation(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    /// The relay is a pipe for tool dispatch: the request reaches the harness
    /// field-for-field and the harness's `call_id` comes back unaltered. A
    /// silent rewrite of the forwarded body would break the client's browser
    /// pane with every authorization test still green.
    #[tokio::test]
    async fn dispatch_tool_forwards_request_unmodified_and_returns_harness_call_id() {
        let seen_dispatch: Arc<std::sync::Mutex<Option<CallToolRequest>>> = Arc::default();
        let addr = spawn_mock_harness(MockHarness {
            seen_dispatch: seen_dispatch.clone(),
            ..Default::default()
        })
        .await;
        let service = make_service_dialing(addr).await;
        service
            .state
            .record_conversation_owner("ws.conv", "row-ws")
            .await;
        let sent = CallToolRequest {
            name: "Bash".into(),
            input_json: r#"{"cmd":"ls"}"#.into(),
            conversation_id: "ws.conv".into(),
        };
        let resp = service
            .dispatch_tool(req_with_workspace(sent.clone(), "ws"))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().call_id, "call-42");
        assert_eq!(*seen_dispatch.lock().unwrap(), Some(sent));
    }

    /// Result frames are relayed 1:1, in order, with no filtering,
    /// truncation, or reordering, and the subscribe request itself reaches
    /// the harness untouched.
    #[tokio::test]
    async fn await_tool_result_streams_harness_frames_unmodified() {
        let seen_await: Arc<std::sync::Mutex<Option<AwaitToolResultRequest>>> = Arc::default();
        let addr = spawn_mock_harness(MockHarness {
            seen_await: seen_await.clone(),
            ..Default::default()
        })
        .await;
        let service = make_service_dialing(addr).await;
        service
            .state
            .record_conversation_owner("ws.conv", "row-ws")
            .await;
        let sent = AwaitToolResultRequest {
            call_id: "call-42".into(),
            conversation_id: "ws.conv".into(),
        };
        let stream = service
            .await_tool_result(req_with_workspace(sent.clone(), "ws"))
            .await
            .unwrap()
            .into_inner();
        let frames: Vec<ToolResultFrame> = stream.map(|f| f.unwrap()).collect().await;
        assert_eq!(
            frames,
            vec![
                ToolResultFrame {
                    frame: Some(proto_common::tool_result_frame::Frame::Stdout("out".into())),
                },
                ToolResultFrame {
                    frame: Some(proto_common::tool_result_frame::Frame::Complete(
                        proto_common::ToolComplete::default(),
                    )),
                },
            ],
            "every harness frame reaches the caller in order and unaltered"
        );
        assert_eq!(*seen_await.lock().unwrap(), Some(sent));
    }

    /// Cancel forwards on the call_id alone and reports the harness's
    /// answer. Unlike dispatch and await, this handler applies no
    /// conversation guard, so no cache is primed here.
    #[tokio::test]
    async fn cancel_tool_forwards_call_id_and_returns_harness_cancelled() {
        let seen_cancel: Arc<std::sync::Mutex<Option<CancelToolRequest>>> = Arc::default();
        let addr = spawn_mock_harness(MockHarness {
            seen_cancel: seen_cancel.clone(),
            ..Default::default()
        })
        .await;
        let service = make_service_dialing(addr).await;
        let sent = CancelToolRequest {
            call_id: "call-42".into(),
        };
        let resp = service
            .cancel_tool(req_with_workspace(sent.clone(), "ws"))
            .await
            .unwrap();
        assert!(resp.into_inner().cancelled);
        assert_eq!(*seen_cancel.lock().unwrap(), Some(sent));
    }

    #[tokio::test]
    async fn get_conversation_history_rejects_empty_id() {
        let service = make_service_with(fixture_verifier()).await;
        let req = req_with_workspace(
            GetConversationHistoryRequest {
                conversation_id: String::new(),
                limit: None,
                owner: String::new(),
            },
            "ws",
        );
        let err = service.get_conversation_history(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn list_conversations_rejects_body_workspace_conflict() {
        let service = make_service_with(fixture_verifier()).await;
        let req = req_with_workspace(
            ListConversationsRequest {
                workspace: "beta".into(),
                owner: String::new(),
            },
            "alpha",
        );
        let err = tokio::time::timeout(Duration::from_secs(1), service.list_conversations(req))
            .await
            .expect("workspace conflict must be rejected without dialing the harness")
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn list_workspaces_rejects_missing_extension() {
        let service = make_service_with(fixture_verifier()).await;
        let req = Request::new(ListWorkspacesRequest {});
        let err = service.list_workspaces(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    /// Service whose state carries a credential menu for `hello-world`.
    async fn make_service_with_credentials() -> GatewayService {
        let menu = crate::credentials::CredentialMenu::parse_for_tests(
            "hello-world:\n  - name: ssh-credentials\n    grants:\n      github:\n        secret: k\n",
        );
        let state = Arc::new(
            GatewayState::new(fixture_verifier(), None, "default".into()).with_credentials(menu),
        );
        *state.grants().write().await = fixture_grants();
        GatewayService::new(state)
    }

    #[tokio::test]
    async fn list_grants_answers_the_verified_workspace_menu() {
        let service = make_service_with_credentials().await;
        let req = req_with_workspace(
            ListGrantsRequest {
                workspace: String::new(),
            },
            "hello-world",
        );
        let resp = service.list_grants(req).await.unwrap().into_inner();
        assert_eq!(resp.toolsets.len(), 1);
        assert_eq!(resp.toolsets[0].toolset, "ssh-credentials");
        assert_eq!(resp.toolsets[0].grants, vec!["github".to_string()]);
    }

    #[tokio::test]
    async fn list_grants_rejects_a_conflicting_body_workspace() {
        let service = make_service_with_credentials().await;
        let req = req_with_workspace(
            ListGrantsRequest {
                workspace: "alpha".into(),
            },
            "hello-world",
        );
        let err = service.list_grants(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn list_grants_for_a_menuless_workspace_is_empty_not_an_error() {
        let service = make_service_with_credentials().await;
        let req = req_with_workspace(
            ListGrantsRequest {
                workspace: String::new(),
            },
            "ws",
        );
        let resp = service.list_grants(req).await.unwrap().into_inner();
        assert!(resp.toolsets.is_empty());
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

    // CancelTurn travels back through the gateway to the harness as a pure
    // relay, applying the same guard-then-forward contract as its sibling
    // lifecycle RPCs (delete_conversation / get_turn_state): a CancelTurn with
    // no conversation_id is rejected at the gateway, never forwarded blind.
    #[tokio::test]
    async fn cancel_turn_rejects_empty_conversation_id() {
        // Materiality: drop the empty-id guard on the gateway's cancel_turn
        // forwarder -> an unkeyed CancelTurn is dialed at the harness
        // instead of failing fast with InvalidArgument.
        let service = make_service_with(fixture_verifier()).await;
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
