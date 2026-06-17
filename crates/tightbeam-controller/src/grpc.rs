use crate::state::{ControllerState, PendingTurn};
use futures::StreamExt;
use serde_json::Value;
use shared::auth::{extract_bearer_token, TokenVerifier};
use shared::client_signature::ClientSignatureVerifier;
use std::sync::Arc;

/// Seconds to keep the channel's outbound side open after the client
/// half-closes. Just under the 60s default gRPC client deadline.
const CHANNEL_DRAIN_SECS: u64 = 55;

/// Fires `unregister_channel` after a drain delay when the held
/// outbound stream is dropped. Used by `channel_receive` to tie
/// channel-registry lifetime to the gRPC response stream's lifetime
/// instead of a wall-clock timer. The drain delay lets multi-frame
/// outbound replies that were queued at the moment of drop finish.
struct ChannelDropGuard {
    state: Arc<ControllerState>,
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

use tightbeam_proto::convert::{
    chunk_to_turn_event, proto_message_to_provider, proto_tool_call_to_provider,
    provider_message_to_proto,
};
use tightbeam_proto::tightbeam_controller_server::TightbeamController;
use tightbeam_proto::{
    channel_inbound, channel_outbound, turn_result_chunk, CallToolRequest, CallToolResponse,
    ChannelAck, ChannelInbound, ChannelIngestAck, ChannelIngestRequest, ChannelOutbound,
    ChannelReceiveRequest, ChannelSend, DeleteConversationRequest, DeleteConversationResponse,
    GetConversationHistoryRequest, GetConversationHistoryResponse, GetTurnRequest, HistoryEntry,
    ListConversationsRequest, ListConversationsResponse, ListWorkspacesRequest,
    ListWorkspacesResponse, MintConversationRequest, MintConversationResponse,
    RedeemEnrollmentRequest, RedeemEnrollmentResponse, SendServerNotificationRequest,
    SendServerNotificationResponse, SendServerRequestAndAwaitRequest,
    SendServerRequestAndAwaitResponse, SubscribeRequest, ToolListUpdate, TurnAck, TurnAssignment,
    TurnComplete, TurnEvent, TurnRequest, TurnResultChunk, TurnRole, TurnState, UserMessage,
    WatchToolsRequest,
};
use tightbeam_providers::merge::merge_rfc7396;
use tightbeam_providers::types as provider;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

/// True only when the turn ended naturally AND was not a delegated subagent
/// call. The reply_channel SendMessage is user-facing: orchestrator turns
/// reach the user, delegate turns return to the orchestrator (not the user).
/// `stop_reason` is the proto wire value of `StopReason::EndTurn` (1).
fn should_send_user_facing_reply(stop_reason: i32, role: Option<TurnRole>) -> bool {
    stop_reason == tightbeam_proto::StopReason::EndTurn as i32
        && !matches!(role, Some(TurnRole::Delegate))
}

/// Pick the conversation-history scope for a turn. Only a (Delegate, Some(id))
/// pair produces a delegate-scoped view; every other combination falls back to
/// the orchestrator scope.
fn history_scope_for_role<'a>(
    role: Option<TurnRole>,
    correlation_id: Option<&'a str>,
) -> crate::conversation::HistoryScope<'a> {
    match (role, correlation_id) {
        (Some(TurnRole::Delegate), Some(id)) => crate::conversation::HistoryScope::Delegate(id),
        _ => crate::conversation::HistoryScope::Orchestrator,
    }
}

/// Returns the request-body model name if it's a non-empty string, else None.
/// `params.model` is wire-optional but treats empty-string the same as absent.
fn non_empty_request_model(model: Option<&str>) -> Option<&str> {
    model.filter(|m| !m.is_empty())
}

/// True if a list_conversations request body's `workspace` field conflicts
/// with the auth-verified workspace claim. An empty body field is accepted
/// (informational only); a non-empty body field MUST equal the claim.
fn workspace_claim_conflicts(body_ws: &str, verified_ws: &str) -> bool {
    !body_ws.is_empty() && body_ws != verified_ws
}

/// True when the returned entries are a strict prefix of the conversation
/// log — `total_seq` exceeds the slice length, so older entries were
/// truncated to honor `limit`. Equality means the full conversation fits.
fn snapshot_was_truncated(entries_len: usize, total_seq: u64) -> bool {
    (entries_len as u64) < total_seq
}

#[allow(clippy::result_large_err)] // tonic::Status is the gRPC-shaped error this layer returns
#[allow(clippy::unnecessary_wraps)] // empty-case used to return Err; signature kept Result to leave the door open for future validation
fn assistant_message_from_complete(complete: &TurnComplete) -> Result<provider::Message, Status> {
    // Preserve every supported ContentBlock variant (Text, Thinking, Image).
    // Unknown variants are logged as warnings — never silently dropped.
    use tightbeam_proto::convert::proto_content_to_provider;
    let mut content: Vec<provider::ContentBlock> = Vec::new();
    for cb in &complete.content {
        match proto_content_to_provider(cb) {
            Some(block) => content.push(block),
            None => {
                // The block variant is either unknown or FileIncoming (which
                // must be resolved by the controller before reaching the LLM).
                // FileIncoming here suggests a code path missed the resolution
                // step. Log instead of silently dropping.
                tracing::warn!(
                    "assistant_message_from_complete: unhandled ContentBlock variant dropped: {:?}",
                    cb.block
                );
            }
        }
    }

    // Drop whitespace-only Text / Thinking blocks. Anthropic rejects
    // such blocks on the next turn ("text content blocks must contain
    // non-whitespace text"); leaving them in the log poisons every
    // subsequent turn. Image / FileIncoming have no text to trim.
    content.retain(|block| match block {
        provider::ContentBlock::Text { text } | provider::ContentBlock::Thinking { text } => {
            !text.trim().is_empty()
        }
        _ => true,
    });

    let tool_calls: Vec<provider::ToolCall> = complete
        .tool_calls
        .iter()
        .map(proto_tool_call_to_provider)
        .collect();

    // Model emitted nothing — no content (or only whitespace), no tool
    // calls. Substitute a placeholder so the log stays Anthropic-valid
    // and the transponder's nudge loop sees a normal EndTurn it can act
    // on.
    if content.is_empty() && tool_calls.is_empty() {
        tracing::warn!(
            "TurnComplete had no text content and no tool calls; \
             substituting placeholder to keep conversation Anthropic-valid"
        );
        content.push(provider::ContentBlock::Text {
            text: "(no output)".into(),
        });
    }

    Ok(provider::Message {
        role: "assistant".into(),
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        is_error: None,
    })
}

async fn build_params_json(
    state: &ControllerState,
    model: &str,
    frontmatter_params: Option<&serde_json::Map<String, Value>>,
) -> Option<String> {
    let model_spec = state.get_model_spec(model).await;
    let mut merged = model_spec.and_then(|s| s.params).unwrap_or_default();
    if let Some(fm_params) = frontmatter_params {
        merge_rfc7396(&mut merged, &Value::Object(fm_params.clone()));
    }
    if merged.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&merged)
                .expect("Map<String, Value> serializes deterministically"),
        )
    }
}

/// Pair of K8s TokenReview verifiers for the internal listener — one
/// per audience. The `audience_layer` tower middleware stamps a
/// `RequiredAudience` extension on each request based on its gRPC method
/// path; the handler picks the matching verifier here. Without the
/// layer, `verify_workspace` rejects with `Internal("audience layer not
/// wired")` so a misconfigured listener fails closed.
pub struct InternalVerifierPair {
    pub transponder: Arc<dyn TokenVerifier>,
    pub llm: Arc<dyn TokenVerifier>,
}

/// Per-listener strategy for resolving the caller's workspace.
/// Constructed by the listener wiring in `main.rs`; the handler is
/// listener-agnostic.
pub enum VerificationStrategy {
    /// Internal listener (port 9090): K8s SA token in `authorization`
    /// metadata, verified via TokenReview against one of the two
    /// audiences carried by the `RequiredAudience` extension.
    BearerToken(InternalVerifierPair),
    /// External listener (port 9091): the `signature_layer` tower
    /// middleware has already verified the signed-request envelope
    /// and stamped the workspace on the request extensions. The
    /// handler trusts the extension.
    TrustExtensionsSetByMiddleware,
    /// Misconfigured — no verifier wired. Every authenticated RPC
    /// fails with FailedPrecondition.
    None,
}

pub struct ControllerService {
    state: Arc<ControllerState>,
    strategy: VerificationStrategy,
    signing_key: ed25519_dalek::SigningKey,
    /// External-listener-only handle on the same verifier the signature
    /// middleware uses. `list_workspaces` reads the kid from the request
    /// extension stamped by the middleware and asks the verifier for
    /// that kid's `spec.workspaces`. `None` on the internal listener.
    verifier: Option<Arc<ClientSignatureVerifier>>,
}

impl ControllerService {
    /// Construct a controller service for the internal listener.
    /// `pair` carries the two TokenReview verifiers — one per audience.
    /// `None` means no kube client is available and the controller will
    /// reject all authed RPCs with FailedPrecondition.
    pub fn internal(
        state: Arc<ControllerState>,
        pair: Option<InternalVerifierPair>,
        signing_key: ed25519_dalek::SigningKey,
    ) -> Self {
        let strategy = match pair {
            Some(p) => VerificationStrategy::BearerToken(p),
            None => VerificationStrategy::None,
        };
        Self {
            state,
            strategy,
            signing_key,
            verifier: None,
        }
    }

    /// Construct a controller service for the external listener. The
    /// signature-verifying middleware in `signature_layer` is
    /// responsible for proving the caller's identity; the handler
    /// reads the verified workspace from request extensions.
    pub fn external(
        state: Arc<ControllerState>,
        signing_key: ed25519_dalek::SigningKey,
        verifier: Arc<ClientSignatureVerifier>,
    ) -> Self {
        Self {
            state,
            strategy: VerificationStrategy::TrustExtensionsSetByMiddleware,
            signing_key,
            verifier: Some(verifier),
        }
    }

    async fn verify_workspace<T>(&self, request: &Request<T>) -> Result<String, Status> {
        match &self.strategy {
            VerificationStrategy::BearerToken(pair) => {
                let token = extract_bearer_token(request)?;
                let verifier = pick_verifier(request, pair)?;
                verifier.verify_token(token).await
            }
            VerificationStrategy::TrustExtensionsSetByMiddleware => request
                .extensions()
                .get::<crate::signature_layer::VerifiedWorkspace>()
                .map(|w| w.0.clone())
                .ok_or_else(|| {
                    Status::permission_denied(
                        "missing verified workspace extension; middleware must populate it",
                    )
                }),
            VerificationStrategy::None => Err(Status::failed_precondition(
                "no token verifier configured: workspace identity cannot be established",
            )),
        }
    }
}

/// Enforce that the verified caller owns the turn under operation.
///
/// Returns `NotFound` on mismatch (not `PermissionDenied`) to avoid leaking
/// the existence of cross-workspace turn IDs to an attacker probing for
/// other workspaces' active turns. See OWASP API1:2023 (BOLA) — "exists but
/// not yours" must be indistinguishable from "does not exist" on the wire.
/// The denial reason is captured in the warn-level structured log.
#[allow(clippy::result_large_err)]
fn enforce_caller_owns_turn(
    caller_workspace: &str,
    turn_owner: &str,
    rpc: &'static str,
) -> Result<(), Status> {
    if caller_workspace == turn_owner {
        return Ok(());
    }
    tracing::warn!(
        rpc,
        caller_workspace,
        attempted_owner = turn_owner,
        "cross-workspace turn access denied",
    );
    Err(Status::not_found("turn not found"))
}

/// Pick the verifier matching the request's `RequiredAudience` extension.
/// The audience layer must have run; otherwise the request fails closed
/// with `Internal("audience layer not wired")`.
#[allow(clippy::result_large_err)]
fn pick_verifier<'a, T>(
    request: &Request<T>,
    pair: &'a InternalVerifierPair,
) -> Result<&'a Arc<dyn TokenVerifier>, Status> {
    let required = request
        .extensions()
        .get::<crate::audience_layer::RequiredAudience>()
        .ok_or_else(|| {
            Status::internal(
                "audience layer not wired: internal listener must install RequiredAudienceLayer",
            )
        })?;
    match required {
        crate::audience_layer::RequiredAudience::Transponder => Ok(&pair.transponder),
        crate::audience_layer::RequiredAudience::Llm => Ok(&pair.llm),
    }
}

#[tonic::async_trait]
impl TightbeamController for ControllerService {
    async fn get_turn(
        &self,
        request: Request<GetTurnRequest>,
    ) -> Result<Response<TurnAssignment>, Status> {
        // Caller MUST authenticate via SA token. The LLM Job pod runs
        // with sa-<workspace> so its identity binds to a specific
        // workspace; we verify the dequeued PendingTurn's workspace
        // matches before handing over the assignment (which carries
        // system prompt + conversation history).
        let caller_workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        if req.model_name.is_empty() {
            return Err(Status::invalid_argument(
                "GetTurnRequest.model_name must be set: the LLM Job must declare which model it serves",
            ));
        }
        let model = req.model_name;

        tracing::info!(model = %model, "get_turn: marking job connected");
        self.state.set_job_connected(&model, true).await;
        // Keepalive: the pod just arrived at its long-poll, which proves
        // it's alive and ready. The cleanup sweep won't reap a slot
        // whose last_activity is within KEEPALIVE_IDLE_SECONDS.
        self.state.bump_model_activity(&model).await;

        tracing::info!(model = %model, "get_turn: waiting for pending turn");
        let pending = self
            .state
            .wait_for_turn(&model)
            .await
            .ok_or_else(|| Status::unavailable("controller shutting down"))?;

        // ModelSlot is keyed globally by model name (a model spec may be
        // referenced by multiple workspaces). The workspace-binding check
        // is on the dequeued PendingTurn, not the request — the caller's
        // SA-derived workspace must own the turn it's about to receive.
        enforce_caller_owns_turn(&caller_workspace, &pending.workspace, "get_turn")?;

        tracing::info!(
            model = %model,
            "get_turn: received assignment with {} messages",
            pending.assignment.messages.len()
        );
        self.state
            .set_active_turn(
                &model,
                pending.workspace,
                pending.conversation_id,
                pending.reply_channel,
                pending.role,
                pending.correlation_id,
                pending.system_prompt,
                pending.result_tx,
            )
            .await;

        Ok(Response::new(pending.assignment))
    }

    async fn stream_turn_result(
        &self,
        request: Request<Streaming<TurnResultChunk>>,
    ) -> Result<Response<TurnAck>, Status> {
        tracing::info!("stream_turn_result: entry");

        // Request<Streaming<_>> is not Sync (the body contains a non-Sync
        // Decoder), so we can't hold &request across the verify_workspace
        // await. Decompose first, then synthesize a Request<()> carrying the
        // metadata + extensions for verification.
        let (metadata, extensions, stream) = {
            let metadata = request.metadata().clone();
            let extensions = request.extensions().clone();
            let stream = request.into_inner();
            (metadata, extensions, stream)
        };
        let mut auth_request = Request::new(());
        *auth_request.metadata_mut() = metadata.clone();
        *auth_request.extensions_mut() = extensions;
        let caller_workspace = self.verify_workspace(&auth_request).await?;

        let model = metadata
            .get("x-tightbeam-model")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| Status::invalid_argument("missing x-tightbeam-model metadata header"))?;

        let active = match self
            .state
            .take_active_turn_if_owned(&model, &caller_workspace)
            .await
        {
            Ok(turn) => turn,
            Err(crate::state::TakeTurnError::NoActiveTurn) => {
                return Err(Status::failed_precondition("no active turn"));
            }
            Err(crate::state::TakeTurnError::OwnerMismatch { owner }) => {
                // Slot intact for the legitimate owner. Helper logs the
                // denial and returns Status::not_found.
                return Err(enforce_caller_owns_turn(
                    &caller_workspace,
                    &owner,
                    "stream_turn_result",
                )
                .expect_err("OwnerMismatch implies caller != owner"));
            }
        };

        let mut stream = stream;
        let mut complete_chunk: Option<TurnResultChunk> = None;
        let mut warnings_collected: Vec<String> = Vec::new();

        while let Some(chunk) = stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("stream error: {e}")))?
        {
            match &chunk.chunk {
                Some(turn_result_chunk::Chunk::Complete(_)) => {
                    // Buffer the Complete chunk — don't forward yet.
                    // The workspace sees it only after persist succeeds.
                    complete_chunk = Some(chunk.clone());
                }
                Some(turn_result_chunk::Chunk::Warning(w)) => {
                    warnings_collected.push(w.field.clone());
                    // Warnings are forwarded immediately — they carry
                    // no persist implications.
                    let _ = active.result_tx.send(chunk).await;
                }
                // Delta chunks (ContentDelta, ToolUseStart, ToolUseInput)
                // are forwarded immediately for streaming UX. Only the
                // terminal Complete chunk is deferred.
                _ => {
                    let _ = active.result_tx.send(chunk).await;
                }
            }
        }

        // At this point the stream has ended. Run the persist gate.
        let persist_ok: Result<(), Status> = if let Some(TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Complete(ref complete)),
            ..
        }) = complete_chunk
        {
            let assistant_msg = assistant_message_from_complete(complete).map_err(|e| {
                tracing::error!(
                    workspace = %active.workspace,
                    conversation_id = %active.conversation_id,
                    error = %e,
                    "rejecting malformed TurnComplete",
                );
                e
            })?;
            let tag =
                crate::conversation::derive_tag(active.role, active.correlation_id.as_deref());
            let attribution = crate::conversation::AssistantAttribution {
                model: Some(model.clone()),
                system_prompt_sha256: active
                    .system_prompt
                    .as_deref()
                    .map(crate::conversation::sha256_hex),
                warnings: warnings_collected.clone(),
            };
            let ws = self.state.get_or_create_workspace(&active.workspace).await;
            let conv_arc = ws
                .get_or_create_conversation(&active.conversation_id)
                .await
                .map_err(|e| {
                    tracing::error!(
                        workspace = %active.workspace,
                        conversation_id = %active.conversation_id,
                        error = %e,
                        "failed to load conversation for assistant append",
                    );
                    Status::internal(format!("conversation store unavailable: {e}"))
                })?;
            let mut conv = conv_arc.write().await;
            conv.append_assistant_tagged(assistant_msg, tag, attribution)
                .await
                .map_err(|e| {
                    tracing::error!(
                        workspace = %active.workspace,
                        conversation_id = %active.conversation_id,
                        error = %e,
                        "failed to append assistant message to conversation log",
                    );
                    Status::internal(format!("conversation append failed: {e}"))
                })?;

            if should_send_user_facing_reply(complete.stop_reason, active.role) {
                if let Some(ref channel_key) = active.reply_channel {
                    let outbound = ChannelOutbound {
                        command: Some(channel_outbound::Command::SendMessage(ChannelSend {
                            content: complete.content.clone(),
                            conversation_id: active.conversation_id.clone(),
                        })),
                    };
                    self.state.send_to_channel(channel_key, outbound).await;
                    // IDLE follows SendMessage on the same mpsc — single
                    // FIFO queue guarantees clients see the assistant
                    // bubble before the indicator collapses.
                    self.state
                        .set_and_broadcast_turn_state(
                            channel_key,
                            &active.conversation_id,
                            TurnState::Idle,
                        )
                        .await;
                }
            }
            // Mark the conversation as touched after a successful append
            // so MRU ordering reflects the assistant reply too, not just
            // the user message that triggered the turn.
            ws.touch(&active.conversation_id).await;

            Ok(())
        } else {
            // No Complete chunk — the LLM job streamed nothing to persist.
            // The workspace stream will simply end without a terminal event.
            Ok(())
        };

        match persist_ok {
            Ok(()) => {
                // Persist succeeded. Forward the buffered Complete chunk
                // to the workspace, then close the stream.
                if let Some(c) = complete_chunk {
                    let _ = active.result_tx.send(c).await;
                }
                drop(active.result_tx);
                // Keepalive: bump on persist-success only. A malformed
                // Complete that hit the Err branch above must NOT extend
                // the lease on a wedged Job.
                self.state.bump_model_activity(&model).await;
                Ok(Response::new(TurnAck {}))
            }
            Err(status) => {
                // Persist failed. Send a TurnError to the workspace so
                // the agent runtime sees the failure, then close the stream.
                let error_chunk = TurnResultChunk {
                    chunk: Some(turn_result_chunk::Chunk::Error(
                        tightbeam_proto::TurnError {
                            code: status.code() as i32,
                            message: status.message().to_string(),
                        },
                    )),
                };
                let _ = active.result_tx.send(error_chunk).await;
                drop(active.result_tx);
                // Persist failure leaves the UI stranded in WORKING
                // otherwise. Reset to IDLE so the user can re-send.
                if let Some(ref channel_key) = active.reply_channel {
                    self.state
                        .set_and_broadcast_turn_state(
                            channel_key,
                            &active.conversation_id,
                            TurnState::Idle,
                        )
                        .await;
                }
                Err(status)
            }
        }
    }

    type TurnStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<TurnEvent, Status>> + Send>>;

    async fn turn(
        &self,
        request: Request<TurnRequest>,
    ) -> Result<Response<Self::TurnStream>, Status> {
        tracing::info!("turn: entry");
        let workspace = self.verify_workspace(&request).await?;
        let params = request.into_inner();

        if params.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "TurnRequest.conversation_id must be set — call MintConversation to obtain one",
            ));
        }
        if !params.conversation_id.starts_with(&format!("{workspace}.")) {
            return Err(Status::permission_denied(
                "TurnRequest.conversation_id workspace prefix does not match caller",
            ));
        }
        let conversation_id = params.conversation_id.clone();

        // Per-turn system prompt: each TurnRequest carries the system prompt the
        // dispatching call was running under. We do NOT retain it on the
        // workspace because orchestrator and delegate turns interleave under
        // different prompts; sharing one slot would cross-contaminate.
        //
        // The pre-strip value (`system`) is what gets hashed onto the audit log
        // entry — auditors hash canonical persona files directly with
        // `sha256sum` and the values match. The post-strip value
        // (`dispatch_system`) is what the LLM Job actually receives; the
        // frontmatter is metadata, not prompt content.
        let system = params.system.clone();
        let (dispatch_system, fm) = match system.as_deref() {
            Some(s) => {
                let (body, fm) = crate::conversation::strip_frontmatter(s);
                (Some(body), fm)
            }
            None => (None, crate::conversation::Frontmatter::default()),
        };

        let role = params.role.and_then(|r| TurnRole::try_from(r).ok());
        let scope = history_scope_for_role(role, params.correlation_id.as_deref());

        let ws = self.state.get_or_create_workspace(&workspace).await;

        // Model resolution order:
        //   1. Frontmatter `model: inherit` → most recent assistant model in
        //      the current scope; falls through to (4) if no prior turn.
        //   2. Frontmatter `model: <name>` (any other value) → that name.
        //   3. `params.model` on the inbound TurnRequest (if non-empty).
        //   4. Reserved name `default` if registered, else alphabetic-first.
        let model = match fm.model.as_deref() {
            Some("inherit") => {
                let conv_arc = ws
                    .get_or_create_conversation(&conversation_id)
                    .await
                    .map_err(|e| {
                        Status::internal(format!("failed to load conversation: {e}"))
                    })?;
                let conv = conv_arc.read().await;
                let inherited = conv.last_assistant_model(scope);
                drop(conv);
                match inherited {
                    Some(m) => m,
                    None => self
                        .state
                        .default_or_alphabetic_first()
                        .await
                        .ok_or_else(|| {
                            Status::failed_precondition(
                                "model: inherit had no prior turn and no fallback model is registered",
                            )
                        })?,
                }
            }
            Some(other) => other.to_string(),
            None => match non_empty_request_model(params.model.as_deref()) {
                Some(m) => m.to_string(),
                None => self
                    .state
                    .default_or_alphabetic_first()
                    .await
                    .ok_or_else(|| {
                        Status::failed_precondition(
                            "no model specified and no models registered: pass `model:` in frontmatter, set `model` on TurnRequest, or register at least one Model",
                        )
                    })?,
            },
        };

        tracing::info!(model = %model, workspace = %workspace, "turn: acquiring conversation write lock");
        let conv_arc = ws
            .get_or_create_conversation(&conversation_id)
            .await
            .map_err(|e| Status::internal(format!("failed to load conversation: {e}")))?;
        let mut conv = conv_arc.write().await;
        tracing::info!("turn: lock acquired");

        let job_action = self.state.check_job_needed(&model).await;
        if matches!(job_action, crate::state::JobAction::NoModelSpec) {
            return Err(Status::failed_precondition(format!(
                "no Model configured for '{model}'"
            )));
        }
        if let crate::state::JobAction::NoProviderSpec(ref provider_name) = job_action {
            return Err(Status::failed_precondition(format!(
                "Model '{model}' references missing provider '{provider_name}'"
            )));
        }

        match &job_action {
            crate::state::JobAction::AlreadyConnected => {
                tracing::debug!(model = %model, "reusing existing LLM job");
            }
            crate::state::JobAction::NoKubeClient => {
                tracing::error!(model = %model, "BUG: no kube client at request time");
            }
            crate::state::JobAction::Create(_)
            | crate::state::JobAction::NoModelSpec
            | crate::state::JobAction::NoProviderSpec(_) => {}
        }

        let incoming: Vec<provider::Message> = params
            .messages
            .iter()
            .map(proto_message_to_provider)
            .collect();

        let rollback_len = conv.len();

        let incoming_tag = crate::conversation::derive_tag(role, params.correlation_id.as_deref());

        conv.append_many_tagged(incoming, incoming_tag)
            .await
            .map_err(|e| Status::internal(format!("conversation append: {e}")))?;

        let history = conv.history_for_provider(scope);

        if let crate::state::JobAction::Create(create_spec) = job_action {
            let client = self.state.kube_client().unwrap();
            let addr = self.state.controller_addr().to_owned();
            let ns = self.state.namespace().to_owned();
            let image = self.state.llm_job_image().to_owned();

            tracing::info!(model = %model, "turn: no LLM Job connected, creating one");
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                crate::job::create_llm_job(
                    client,
                    &model,
                    &create_spec.model,
                    &create_spec.provider,
                    &image,
                    &addr,
                    &ns,
                    &workspace,
                    self.state.scheduling(),
                ),
            )
            .await
            {
                Ok(Ok(name)) => {
                    tracing::info!(job = %name, "turn: LLM Job created");
                    // Register the spawn so dedup sees it on the next
                    // CallTool, and so the cleanup loop can reap it
                    // after idle. Initial bump prevents reap before
                    // the pod has had a chance to connect.
                    self.state.set_active_llm_job(&model, Some(name)).await;
                    self.state.bump_model_activity(&model).await;
                }
                Ok(Err(e)) => {
                    tracing::error!("turn: k8s API rejected Job creation: {e}");
                    conv.truncate(rollback_len).await;
                    return Err(Status::internal(format!("failed to create LLM Job: {e}")));
                }
                Err(_) => {
                    tracing::error!("turn: k8s API timed out creating Job (10s)");
                    conv.truncate(rollback_len).await;
                    return Err(Status::internal(
                        "k8s API timed out creating LLM Job".to_string(),
                    ));
                }
            }

            tracing::info!(model = %model, "turn: waiting for Job to connect");
            let connected = self
                .state
                .wait_for_job_connect(&model, std::time::Duration::from_secs(30))
                .await;
            if !connected {
                conv.truncate(rollback_len).await;
                return Err(Status::deadline_exceeded(
                    "LLM Job did not connect within 30s",
                ));
            }
        }

        drop(conv);
        tracing::info!("turn: conversation lock released");

        tracing::info!("turn: building assignment");

        let proto_messages: Vec<_> = history.iter().map(provider_message_to_proto).collect();

        let params_json = build_params_json(&self.state, &model, fm.params.as_ref()).await;

        let assignment = TurnAssignment {
            // dispatch_system is the post-frontmatter-strip body; the LLM Job
            // sees this. Frontmatter is metadata (e.g., model selection), not
            // prompt content.
            system: dispatch_system,
            tools: params.tools,
            messages: proto_messages,
            params_json,
        };

        let (result_tx, result_rx) = mpsc::channel(64);
        let pending = PendingTurn {
            assignment,
            result_tx,
            workspace,
            conversation_id: conversation_id.clone(),
            reply_channel: params.reply_channel,
            role,
            correlation_id: params.correlation_id,
            // system is the pre-strip value; the audit hash on the assistant
            // log entry is computed from this so external auditors can match
            // log entries to canonical persona files via `sha256sum`.
            system_prompt: system,
        };

        tracing::info!(model = %model, "turn: enqueueing turn");
        self.state
            .enqueue_turn(&model, pending)
            .await
            .map_err(Status::internal)?;
        tracing::info!("turn: enqueued, returning stream");

        #[allow(clippy::result_large_err)]
        let event_stream = ReceiverStream::new(result_rx)
            .map(|chunk| -> Result<TurnEvent, Status> { Ok(chunk_to_turn_event(chunk)) });

        Ok(Response::new(Box::pin(event_stream)))
    }

    async fn mint_conversation(
        &self,
        request: Request<MintConversationRequest>,
    ) -> Result<Response<MintConversationResponse>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let ws = self.state.get_or_create_workspace(&workspace).await;
        let conversation_id = ws
            .mint_conversation()
            .await
            .map_err(|e| Status::internal(format!("failed to mint conversation: {e}")))?;
        Ok(Response::new(MintConversationResponse { conversation_id }))
    }

    async fn list_conversations(
        &self,
        request: Request<ListConversationsRequest>,
    ) -> Result<Response<ListConversationsResponse>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        // The verifier-confirmed workspace claim wins over the request body;
        // the body's `workspace` is informational only and must agree.
        if workspace_claim_conflicts(&req.workspace, &workspace) {
            return Err(Status::permission_denied(
                "workspace claim does not match request body",
            ));
        }
        let ws = self.state.get_or_create_workspace(&workspace).await;
        let summaries = ws.list_conversation_summaries().await;
        let conversations: Vec<tightbeam_proto::ConversationSummary> = summaries
            .into_iter()
            .map(|(id, ts, name)| tightbeam_proto::ConversationSummary {
                conversation_id: id,
                last_touched_ms_epoch: ts,
                name,
            })
            .collect();
        Ok(Response::new(ListConversationsResponse { conversations }))
    }

    async fn delete_conversation(
        &self,
        request: Request<DeleteConversationRequest>,
    ) -> Result<Response<DeleteConversationResponse>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "DeleteConversationRequest.conversation_id required",
            ));
        }
        let ws = self.state.get_or_create_workspace(&workspace).await;
        if !ws.owns_conversation(&req.conversation_id).await {
            return Err(Status::permission_denied(
                "conversation_id does not belong to caller's workspace",
            ));
        }
        ws.delete_conversation(&req.conversation_id)
            .await
            .map_err(|e| Status::internal(format!("failed to delete conversation events: {e}")))?;
        Ok(Response::new(DeleteConversationResponse {}))
    }

    async fn set_conversation_name(
        &self,
        request: Request<tightbeam_proto::SetConversationNameRequest>,
    ) -> Result<Response<tightbeam_proto::SetConversationNameResponse>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "SetConversationNameRequest.conversation_id required",
            ));
        }
        if req.name.chars().count() > crate::conversation::MAX_CONVERSATION_NAME_CHARS {
            return Err(Status::invalid_argument(format!(
                "name exceeds {}-character limit",
                crate::conversation::MAX_CONVERSATION_NAME_CHARS,
            )));
        }
        let ws = self.state.get_or_create_workspace(&workspace).await;
        if !ws.owns_conversation(&req.conversation_id).await {
            return Err(Status::permission_denied(
                "conversation_id does not belong to caller's workspace",
            ));
        }
        ws.set_conversation_name(&req.conversation_id, &req.name)
            .await
            .map_err(|e| Status::internal(format!("failed to persist conversation name: {e}")))?;
        Ok(Response::new(
            tightbeam_proto::SetConversationNameResponse {},
        ))
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
        let verifier = self.verifier.as_ref().ok_or_else(|| {
            Status::failed_precondition(
                "list_workspaces is external-listener-only; no verifier wired",
            )
        })?;
        let workspaces = verifier
            .get_workspaces_for_kid(&kid)
            .await
            .ok_or_else(|| Status::permission_denied("client registration not found"))?;
        Ok(Response::new(ListWorkspacesResponse { workspaces }))
    }

    type ChannelStreamStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChannelOutbound, Status>> + Send>>;

    async fn channel_stream(
        &self,
        request: Request<Streaming<ChannelInbound>>,
    ) -> Result<Response<Self::ChannelStreamStream>, Status> {
        // ChannelStream is internal-listener-only (rejected by the
        // signature middleware on the external listener — streaming
        // requests can't pass the body-collect verify). Auth here is
        // K8s SA token. The verified workspace MUST match the workspace
        // the client claims in ChannelRegister; mismatch means an
        // in-cluster pod is trying to inject into a different
        // workspace's stream.
        //
        // Extract the bearer token BEFORE consuming the request; the
        // Streaming<ChannelInbound> inner type isn't Sync, so we can't
        // borrow the Request across `await`. We re-implement the
        // BearerToken-strategy verify inline.
        let caller_workspace = match &self.strategy {
            VerificationStrategy::BearerToken(pair) => {
                let token = extract_bearer_token(&request)?.to_string();
                let verifier = pick_verifier(&request, pair)?;
                verifier.verify_token(&token).await?
            }
            VerificationStrategy::TrustExtensionsSetByMiddleware => request
                .extensions()
                .get::<crate::signature_layer::VerifiedWorkspace>()
                .map(|w| w.0.clone())
                .ok_or_else(|| {
                    Status::permission_denied(
                        "missing verified workspace extension; middleware must populate it",
                    )
                })?,
            VerificationStrategy::None => {
                return Err(Status::failed_precondition(
                    "no token verifier configured: workspace identity cannot be established",
                ));
            }
        };
        let mut stream = request.into_inner();
        let state = self.state.clone();

        let first = stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("stream error: {e}")))?
            .ok_or_else(|| Status::invalid_argument("empty stream"))?;

        let (adapter_hint, workspace) = match first.event {
            Some(channel_inbound::Event::Register(reg)) => {
                let claimed = reg.workspace.unwrap_or_default();
                if claimed.is_empty() {
                    return Err(Status::invalid_argument(
                        "ChannelRegister must include workspace",
                    ));
                }
                if claimed != caller_workspace {
                    return Err(Status::permission_denied(
                        "ChannelRegister.workspace does not match caller's authenticated workspace",
                    ));
                }
                (reg.adapter_hint, claimed)
            }
            _ => {
                return Err(Status::invalid_argument(
                    "first message must be ChannelRegister",
                ));
            }
        };

        let _ = state.get_or_create_workspace(&workspace).await;

        let (tx, rx) = mpsc::channel(16);
        // Mint the server-side channel_id. Send it back as the first
        // ChannelOutbound frame (ChannelAck) so the adapter knows what
        // to echo on subsequent UserMessage frames as well as on any
        // out-of-band ChannelIngest from external clients sharing the
        // same workspace (in-cluster adapters typically don't use that
        // path, but the contract is uniform).
        let channel_id = state
            .mint_channel(workspace.clone(), adapter_hint, tx.clone())
            .await;
        let channel_id_for_drop = channel_id.clone();
        let channel_id_for_loop = channel_id.clone();

        if let Err(e) = tx
            .send(ChannelOutbound {
                command: Some(channel_outbound::Command::Ack(ChannelAck {
                    channel_id: channel_id.clone(),
                })),
            })
            .await
        {
            tracing::warn!(?e, "channel_stream: failed to send initial ChannelAck");
        }

        tokio::spawn(async move {
            while let Ok(Some(inbound)) = stream.message().await {
                match inbound.event {
                    Some(channel_inbound::Event::UserMessage(msg)) => {
                        // In-cluster channel adapters predate the
                        // conversation_id hoist — they don't carry
                        // one yet. Stamp empty so the transponder's
                        // existing fall-through path (treat empty as
                        // "use the per-pod default") still works.
                        // TODO: wire conversation_id resolution here
                        // when in-cluster adapters get a UI surface.
                        state
                            .notify_subscriber(
                                &workspace,
                                UserMessage {
                                    content: msg.content,
                                    sender: msg.sender,
                                    reply_channel: Some(channel_id_for_loop.clone()),
                                    conversation_id: String::new(),
                                },
                            )
                            .await;
                    }
                    Some(channel_inbound::Event::Register(_)) => {}
                    None => {}
                }
            }
            // Keep the outbound channel alive for multi-turn responses.
            // CLI clients half-close immediately; the LLM response may
            // require tool_use → tool_result → end_turn (10-30s).
            tokio::time::sleep(std::time::Duration::from_secs(CHANNEL_DRAIN_SECS)).await;
            state.unregister_channel(&channel_id_for_drop).await;
        });

        #[allow(clippy::result_large_err)]
        let outbound_stream =
            ReceiverStream::new(rx).map(|msg| -> Result<ChannelOutbound, Status> { Ok(msg) });

        Ok(Response::new(Box::pin(outbound_stream)))
    }

    type SubscribeStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<UserMessage, Status>> + Send>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let workspace = self.verify_workspace(&request).await?;

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

    async fn redeem_enrollment(
        &self,
        request: Request<RedeemEnrollmentRequest>,
    ) -> Result<Response<RedeemEnrollmentResponse>, Status> {
        // Unauthenticated by design — the signed enrollment code IS the
        // authentication artifact. Business logic + single-use guard live
        // in `client_store::redeem_for_client` so the security-critical
        // branches are unit-tested behind the `ClientStore` interface.
        let req = request.into_inner();
        let claims = shared::auth::verify_enrollment_code(
            &self.signing_key.verifying_key(),
            &req.enrollment_code,
        )?;

        let kube_client = self
            .state
            .kube_client()
            .ok_or_else(|| Status::failed_precondition("controller has no kube client"))?
            .clone();
        let store = crate::client_store::KubeClientStore::new(kube_client, self.state.namespace());
        let resp = crate::client_store::redeem_for_client(&store, &claims, &req.public_key).await?;

        tracing::info!(
            workspace = %claims.workspace,
            client = %resp.client_name,
            "client enrolled"
        );

        Ok(Response::new(resp))
    }

    async fn get_conversation_history(
        &self,
        request: Request<GetConversationHistoryRequest>,
    ) -> Result<Response<GetConversationHistoryResponse>, Status> {
        // Backs the transponder's `recent_turns` built-in tool. Workspace
        // is derived from the calling SA token via `verify_workspace`;
        // the conversation_id must belong to that workspace (current
        // store layout is per-workspace, so the path naturally scopes).
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument("conversation_id required"));
        }
        if !req.conversation_id.starts_with(&format!("{workspace}.")) {
            return Err(Status::permission_denied(
                "conversation_id workspace prefix does not match caller",
            ));
        }

        let limit = effective_history_limit(req.limit);
        let ws = self.state.get_or_create_workspace(&workspace).await;
        if !ws.owns_conversation(&req.conversation_id).await {
            return Err(Status::not_found(
                "conversation_id not found in this workspace",
            ));
        }
        let conv = ws
            .get_or_create_conversation(&req.conversation_id)
            .await
            .map_err(|e| Status::internal(format!("load conversation: {e}")))?;
        let snap = conv.read().await.snapshot(limit);
        let truncated = snapshot_was_truncated(snap.entries.len(), snap.total_seq);
        let entries: Vec<HistoryEntry> = snap
            .entries
            .into_iter()
            .map(|e| HistoryEntry {
                seq: e.seq,
                ts: e.ts,
                message: Some(provider_message_to_proto(&e.message)),
                tag: e.tag,
            })
            .collect();

        Ok(Response::new(GetConversationHistoryResponse {
            entries,
            total_seq: snap.total_seq,
            truncated,
        }))
    }

    async fn channel_ingest(
        &self,
        request: Request<ChannelIngestRequest>,
    ) -> Result<Response<ChannelIngestAck>, Status> {
        // Workspace is derived from the caller's signature (external
        // listener) or SA token (internal listener) — NEVER from the
        // request body. The caller echoes the server-minted channel_id
        // received earlier from ChannelReceive's first ChannelAck frame;
        // we verify it belongs to the caller's verified workspace before
        // routing.
        let workspace = self.verify_workspace(&request).await?;
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
            (Some(_), Some(_)) => {
                return Err(Status::invalid_argument(
                    "ChannelIngestRequest must carry exactly one of user_message or client_response",
                ));
            }
            (None, None) => {
                return Err(Status::invalid_argument(
                    "ChannelIngestRequest must carry user_message or client_response",
                ));
            }
            (Some(user_message), None) => {
                let ws = self.state.get_or_create_workspace(&workspace).await;
                // Resolve conversation_id: empty → mint, non-empty →
                // validate workspace ownership.
                let conversation_id = if req.conversation_id.is_empty() {
                    ws.mint_conversation().await.map_err(|e| {
                        Status::internal(format!("failed to mint conversation: {e}"))
                    })?
                } else {
                    if !ws.owns_conversation(&req.conversation_id).await {
                        return Err(Status::permission_denied(
                            "ChannelIngestRequest.conversation_id does not belong to caller's workspace",
                        ));
                    }
                    req.conversation_id.clone()
                };
                ws.touch(&conversation_id).await;
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
                // The user message has been accepted and routed; the transponder
                // will pick it up momentarily. Move the channel to WORKING so the
                // client renders an active indicator until the assistant message
                // (or a TurnError) lands.
                self.state
                    .set_and_broadcast_turn_state(
                        &req.channel_id,
                        &conversation_id,
                        TurnState::Working,
                    )
                    .await;
                return Ok(Response::new(ChannelIngestAck {
                    channel_id: req.channel_id,
                    conversation_id,
                }));
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
                // (timeout already fired). Warn-log but do not error —
                // the client may have raced our cleanup.
                if !self
                    .state
                    .deliver_client_response(&req.channel_id, &cr.request_id, outcome)
                    .await
                {
                    tracing::warn!(
                        channel_id = %req.channel_id,
                        request_id = %cr.request_id,
                        "channel_ingest: ClientResponse for unknown/expired request_id"
                    );
                }
            }
        }
        // ClientResponse path does not touch a conversation — empty id
        // in the ack is the honest signal that no conversation was
        // routed by this call.
        Ok(Response::new(ChannelIngestAck {
            channel_id: req.channel_id,
            conversation_id: String::new(),
        }))
    }

    type ChannelReceiveStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChannelOutbound, Status>> + Send>>;

    async fn channel_receive(
        &self,
        request: Request<ChannelReceiveRequest>,
    ) -> Result<Response<Self::ChannelReceiveStream>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        let _ = self.state.get_or_create_workspace(&workspace).await;

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
        let workspace = self.verify_workspace(&request).await?;
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
        // 1:1 pass-through. tonic Streaming<T> is already a futures::Stream<Item=Result<T, Status>>.
        Ok(Response::new(Box::pin(upstream)))
    }

    async fn call_tool(
        &self,
        request: Request<CallToolRequest>,
    ) -> Result<Response<CallToolResponse>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        let mut client = self
            .state
            .transponder_clients()
            .get(&workspace)
            .await
            .map_err(|e| Status::unavailable(format!("transponder unavailable: {e}")))?;
        client.as_mut().call_tool(req).await
    }

    /// Internal-only: transponder asks the controller to push a
    /// fire-and-forget `ServerRequest` onto a channel. Workspace claim is
    /// the transponder's SA-token audience; we verify it owns the
    /// supplied channel_id before dispatching.
    async fn send_server_notification(
        &self,
        request: Request<SendServerNotificationRequest>,
    ) -> Result<Response<SendServerNotificationResponse>, Status> {
        let workspace = self.verify_workspace(&request).await?;
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
        let workspace = self.verify_workspace(&request).await?;
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
        // Clamp timeout. 0 → 30s default; cap at 300s.
        let secs = if req.timeout_seconds == 0 {
            30
        } else {
            req.timeout_seconds.min(300)
        };
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
            Ok(crate::state::ServerRequestOutcome::Result(s)) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: s,
                    error: None,
                    timed_out: false,
                    unknown_channel: false,
                    unsupported_method: false,
                }))
            }
            Ok(crate::state::ServerRequestOutcome::Error(e)) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: Some(e),
                    timed_out: false,
                    unknown_channel: false,
                    unsupported_method: false,
                }))
            }
            Err(crate::state::ServerRequestError::Timeout) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: None,
                    timed_out: true,
                    unknown_channel: false,
                    unsupported_method: false,
                }))
            }
            Err(crate::state::ServerRequestError::UnknownChannel) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: None,
                    timed_out: false,
                    unknown_channel: true,
                    unsupported_method: false,
                }))
            }
            Err(crate::state::ServerRequestError::UnsupportedMethod) => {
                Ok(Response::new(SendServerRequestAndAwaitResponse {
                    result_json: String::new(),
                    error: None,
                    timed_out: false,
                    unknown_channel: false,
                    unsupported_method: true,
                }))
            }
            Err(crate::state::ServerRequestError::SendFailed)
            | Err(crate::state::ServerRequestError::Disconnected) => Err(Status::aborted(
                "channel disconnected before client responded",
            )),
        }
    }
}

/// Server-side clamp for `GetConversationHistoryRequest.limit`. `None`
/// or `Some(0)` → no limit (snapshot returns full log); positive
/// values are clamped to `MAX_HISTORY_LIMIT`. Extracted so the
/// clamping behavior is unit-testable.
const MAX_HISTORY_LIMIT: usize = 500;

fn effective_history_limit(requested: Option<u32>) -> Option<usize> {
    match requested {
        None | Some(0) => None,
        Some(n) => Some((n as usize).min(MAX_HISTORY_LIMIT)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ControllerState;
    use shared::auth::TokenVerifier;

    #[test]
    fn enforce_caller_owns_turn_ok_on_match() {
        let result = enforce_caller_owns_turn("ws-a", "ws-a", "test_rpc");
        assert!(result.is_ok());
    }

    #[test]
    fn enforce_caller_owns_turn_denies_on_mismatch() {
        let err = enforce_caller_owns_turn("ws-a", "ws-b", "test_rpc")
            .expect_err("mismatch must return Err");
        assert_eq!(err.code(), tonic::Code::NotFound);
        assert_eq!(err.message(), "turn not found");
    }

    #[test]
    fn enforce_caller_owns_turn_denies_on_empty_caller() {
        // Defends against a future verify_workspace bug returning empty.
        let err = enforce_caller_owns_turn("", "ws-a", "test_rpc")
            .expect_err("empty caller must return Err");
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    /// Test verifier that ignores the token and returns a fixed workspace
    /// name. Mirrors the integration test helper.
    struct FixedWorkspaceVerifier(String);

    #[tonic::async_trait]
    impl TokenVerifier for FixedWorkspaceVerifier {
        async fn verify_token(&self, _token: &str) -> Result<String, Status> {
            Ok(self.0.clone())
        }
    }

    fn fixed_verifier(name: &str) -> Arc<dyn TokenVerifier> {
        Arc::new(FixedWorkspaceVerifier(name.to_string()))
    }

    /// Test-only InternalVerifierPair where both audiences resolve to the
    /// same fixed workspace name. Production wires two distinct
    /// `K8sTokenVerifier` instances (one per audience).
    fn fixed_pair(name: &str) -> InternalVerifierPair {
        InternalVerifierPair {
            transponder: fixed_verifier(name),
            llm: fixed_verifier(name),
        }
    }

    /// Tonic Request<T> stamped with the transponder audience extension
    /// (matching what the `audience_layer` would do in production). All
    /// non-LLM RPCs go through this helper.
    fn authed<T>(inner: T) -> Request<T> {
        let mut req = Request::new(inner);
        req.metadata_mut()
            .insert("authorization", "Bearer test".parse().unwrap());
        req.extensions_mut()
            .insert(crate::audience_layer::RequiredAudience::Transponder);
        req
    }

    fn make_state() -> Arc<ControllerState> {
        use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
        // `into_path()` releases the TempDir's drop-time cleanup so the
        // directory survives this function's return. Intentional test-scoped
        // leak; process exit handles eventual cleanup.
        let log_dir = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(log_dir));
        Arc::new(ControllerState::new(
            factory,
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
            shared::scheduling::SchedulingConfig::default(),
        ))
    }

    /// Deterministic Ed25519 key for tests. Real callers load from the
    /// chart's mounted Secret; tests don't care about randomness.
    fn fixture_signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
    }

    /// Empty client-signature verifier for tests that exercise the
    /// external-listener path. The signature middleware itself is the
    /// real verifier; these unit tests only need the constructor to be
    /// satisfied.
    fn fixture_client_verifier() -> Arc<ClientSignatureVerifier> {
        Arc::new(ClientSignatureVerifier::new(
            std::time::Duration::from_secs(300),
        ))
    }

    // -- Pure helpers extracted from handler boundaries. Kept tested
    // -- separately so cargo-mutants can prove every branch is reachable.

    #[test]
    fn should_send_user_facing_reply_orchestrator_endturn() {
        // The only case that fires the channel SendMessage: turn ended
        // naturally AND was not a delegate sub-call. Orchestrator turns
        // carry `role: None` (no enum variant); only Delegate is named.
        assert!(should_send_user_facing_reply(
            tightbeam_proto::StopReason::EndTurn as i32,
            None
        ));
        assert!(should_send_user_facing_reply(
            tightbeam_proto::StopReason::EndTurn as i32,
            Some(TurnRole::Unspecified)
        ));
    }

    #[test]
    fn should_send_user_facing_reply_delegate_does_not_forward() {
        // Delegate turns return to the orchestrator's tool-result inbox,
        // never to the user-facing channel.
        assert!(!should_send_user_facing_reply(
            tightbeam_proto::StopReason::EndTurn as i32,
            Some(TurnRole::Delegate)
        ));
    }

    #[test]
    fn should_send_user_facing_reply_non_endturn_does_not_forward() {
        // ToolUse / MaxTokens / Unspecified end states all mean the turn
        // is incomplete from the user's perspective — no SendMessage.
        for stop in [
            tightbeam_proto::StopReason::ToolUse as i32,
            tightbeam_proto::StopReason::MaxTokens as i32,
            tightbeam_proto::StopReason::Unspecified as i32,
        ] {
            assert!(
                !should_send_user_facing_reply(stop, None),
                "stop_reason {stop} must not trigger SendMessage"
            );
            assert!(
                !should_send_user_facing_reply(stop, Some(TurnRole::Delegate)),
                "stop_reason {stop} must not trigger SendMessage"
            );
        }
    }

    #[test]
    fn history_scope_delegate_with_correlation_id_picks_delegate() {
        let scope = history_scope_for_role(Some(TurnRole::Delegate), Some("call-abc"));
        match scope {
            crate::conversation::HistoryScope::Delegate(id) => assert_eq!(id, "call-abc"),
            other => panic!("expected Delegate, got {other:?}"),
        }
    }

    #[test]
    fn history_scope_falls_back_to_orchestrator_when_missing_components() {
        // Each row is a (role, correlation_id) combination that MUST fall
        // back to Orchestrator (kills the `delete match arm` mutant).
        let rows = [
            (Some(TurnRole::Delegate), None),
            (Some(TurnRole::Unspecified), Some("call-abc")),
            (Some(TurnRole::Unspecified), None),
            (None, Some("call-abc")),
            (None, None),
        ];
        for (role, cid) in rows {
            match history_scope_for_role(role, cid) {
                crate::conversation::HistoryScope::Orchestrator => {}
                other => panic!("({role:?}, {cid:?}) expected Orchestrator, got {other:?}"),
            }
        }
    }

    #[test]
    fn non_empty_request_model_filters_empty_strings() {
        // Kills the `delete !` mutant on the filter predicate: empty must
        // become None (so the caller falls through to default_or_alphabetic);
        // non-empty must pass through.
        assert_eq!(non_empty_request_model(None), None);
        assert_eq!(non_empty_request_model(Some("")), None);
        assert_eq!(
            non_empty_request_model(Some("claude-sonnet-4")),
            Some("claude-sonnet-4")
        );
    }

    #[test]
    fn workspace_claim_conflicts_truth_table() {
        // Empty body field is informational — never conflicts.
        assert!(!workspace_claim_conflicts("", "alice"));
        // Body equal to verified workspace agrees.
        assert!(!workspace_claim_conflicts("alice", "alice"));
        // Body different from verified workspace conflicts.
        assert!(workspace_claim_conflicts("bob", "alice"));
    }

    #[test]
    fn snapshot_was_truncated_when_entries_strictly_shorter_than_total() {
        // Strict prefix → truncated.
        assert!(snapshot_was_truncated(5, 10));
        assert!(snapshot_was_truncated(0, 1));
    }

    #[test]
    fn snapshot_was_not_truncated_when_full_log_returned() {
        // Equal → full log returned.
        assert!(!snapshot_was_truncated(5, 5));
        assert!(!snapshot_was_truncated(0, 0));
        // Length-greater-than-total is structurally impossible but kills
        // the `< → >` mutant by pinning the boundary the other way.
        assert!(!snapshot_was_truncated(10, 5));
    }

    #[test]
    fn assistant_message_without_text_or_tool_calls_substitutes_placeholder() {
        use tightbeam_proto::{StopReason, TurnComplete};
        let complete = TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![],
            tool_calls: vec![],
        };
        // Anthropic 400s on empty assistant messages on the next turn,
        // so an empty TurnComplete used to be rejected. The new
        // behaviour substitutes a single-character placeholder so the
        // conversation log stays Anthropic-valid and the transponder's
        // nudge loop sees a normal EndTurn it can act on.
        let msg = assistant_message_from_complete(&complete)
            .expect("empty complete must succeed with placeholder");
        assert_eq!(msg.role, "assistant");
        assert!(msg.tool_calls.is_none());
        let content = msg.content.expect("content must be present");
        assert_eq!(content.len(), 1);
        match &content[0] {
            provider::ContentBlock::Text { text } => {
                assert!(!text.is_empty(), "placeholder text must be non-empty");
            }
            other => panic!("expected text placeholder, got {other:?}"),
        }
    }

    #[test]
    fn assistant_message_with_whitespace_only_text_substitutes_placeholder() {
        // Anthropic rejects assistant content blocks whose text is only
        // whitespace ("text content blocks must contain non-whitespace
        // text") on the next turn. Such blocks must be filtered before
        // they reach the conversation log; once filtered, an otherwise-
        // empty assistant turn falls into the existing placeholder path.
        use tightbeam_proto::{content_block, ContentBlock, StopReason, TextBlock, TurnComplete};
        let complete = TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock {
                    text: "   \n\t  ".into(),
                })),
            }],
            tool_calls: vec![],
        };
        let msg = assistant_message_from_complete(&complete)
            .expect("whitespace-only text must succeed with placeholder");
        let content = msg.content.expect("content must be present");
        assert_eq!(content.len(), 1);
        match &content[0] {
            provider::ContentBlock::Text { text } => {
                assert!(
                    !text.trim().is_empty(),
                    "placeholder text must be non-whitespace",
                );
            }
            other => panic!("expected text placeholder, got {other:?}"),
        }
    }

    #[test]
    fn assistant_message_with_tool_calls_only_passes_validation() {
        use tightbeam_proto::{StopReason, ToolCall, TurnComplete};
        let complete = TurnComplete {
            stop_reason: StopReason::ToolUse as i32,
            content: vec![],
            tool_calls: vec![ToolCall {
                name: "test-tool".into(),
                id: "tc-1".into(),
                input_json: serde_json::json!({"key": "val"}).to_string(),
            }],
        };
        let msg = assistant_message_from_complete(&complete)
            .expect("complete with tool_calls must be Ok");
        assert!(msg.content.is_none(), "content should be None when empty");
        assert!(msg.tool_calls.is_some(), "tool_calls must be present");
        assert_eq!(
            msg.tool_calls.unwrap().len(),
            1,
            "tool_calls count must match"
        );
    }

    #[test]
    fn assistant_message_concatenates_multiple_text_blocks() {
        use tightbeam_proto::{content_block, ContentBlock, StopReason, TextBlock, TurnComplete};
        let complete = TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![
                ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock {
                        text: "first part".into(),
                    })),
                },
                ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock {
                        text: "second part".into(),
                    })),
                },
            ],
            tool_calls: vec![],
        };
        let msg = assistant_message_from_complete(&complete)
            .expect("complete with text content must produce Ok");
        let content = msg.content.expect("message must carry content");
        assert_eq!(content.len(), 2, "expected two text blocks");
        assert_eq!(
            content[0].as_text(),
            Some("first part"),
            "first block preserved"
        );
        assert_eq!(
            content[1].as_text(),
            Some("second part"),
            "second block preserved"
        );
    }

    #[test]
    fn assistant_message_preserves_thinking_only_response() {
        use tightbeam_proto::{content_block, StopReason, ThinkingBlock, TurnComplete};
        let complete = TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![tightbeam_proto::ContentBlock {
                block: Some(content_block::Block::Thinking(ThinkingBlock {
                    text: "deep reasoning".into(),
                })),
            }],
            tool_calls: vec![],
        };
        let msg = assistant_message_from_complete(&complete)
            .expect("thinking-only response must be Ok, not InvalidArgument");
        let content = msg.content.expect("must carry content");
        assert_eq!(content.len(), 1);
        match &content[0] {
            provider::ContentBlock::Thinking { text } => assert_eq!(text, "deep reasoning"),
            _ => panic!("expected Thinking variant"),
        }
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn assistant_message_preserves_thinking_and_text_in_order() {
        use tightbeam_proto::{
            content_block, ContentBlock, StopReason, TextBlock, ThinkingBlock, TurnComplete,
        };
        let complete = TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![
                ContentBlock {
                    block: Some(content_block::Block::Thinking(ThinkingBlock {
                        text: "i think".into(),
                    })),
                },
                ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock {
                        text: "result".into(),
                    })),
                },
            ],
            tool_calls: vec![],
        };
        let msg =
            assistant_message_from_complete(&complete).expect("thinking+text response must be Ok");
        let content = msg.content.expect("must carry content");
        assert_eq!(content.len(), 2);
        match &content[0] {
            provider::ContentBlock::Thinking { text } => assert_eq!(text, "i think"),
            _ => panic!("expected Thinking as first block"),
        }
        match &content[1] {
            provider::ContentBlock::Text { text } => assert_eq!(text, "result"),
            _ => panic!("expected Text as second block"),
        }
    }

    #[tokio::test]
    async fn turn_errors_when_no_verifier_configured() {
        // Replaces the old `turn_without_verifier_uses_default_workspace`
        // test, whose premise (silent fallback to workspace="default") was
        // the reserved-name anti-pattern this change deletes.
        let state = make_state();
        let service = ControllerService::internal(state.clone(), None, fixture_signing_key());

        let result = service
            .turn(authed(TurnRequest {
                system: Some("test".into()),
                tools: vec![],
                messages: vec![],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
                conversation_id: "test-conv".into(),
            }))
            .await;

        let status = match result {
            Ok(_) => panic!("turn must fail when no verifier configured"),
            Err(s) => s,
        };
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert!(
            status.message().contains("no token verifier configured"),
            "got: {:?}",
            status.message()
        );
    }

    #[tokio::test]
    async fn external_service_subscribe_reads_workspace_from_extension() {
        // External listener: signature_layer middleware has already
        // verified the signed-request envelope and stamped the
        // workspace on the request extensions. The handler trusts the
        // extension.
        use crate::signature_layer::VerifiedWorkspace;
        let state = make_state();
        let service = ControllerService::external(
            state.clone(),
            fixture_signing_key(),
            fixture_client_verifier(),
        );

        let mut req = Request::new(SubscribeRequest {});
        req.extensions_mut()
            .insert(VerifiedWorkspace("ws-from-ext".to_string()));

        let response = service.subscribe(req).await;
        assert!(
            response.is_ok(),
            "external strategy must accept request with VerifiedWorkspace extension"
        );
    }

    #[tokio::test]
    async fn external_service_subscribe_rejects_missing_extension() {
        // No middleware → no extension. Handler must refuse rather
        // than silently default a workspace.
        let state = make_state();
        let service =
            ControllerService::external(state, fixture_signing_key(), fixture_client_verifier());

        let req = Request::new(SubscribeRequest {});
        let err = match service.subscribe(req).await {
            Ok(_) => panic!("subscribe must reject missing extension"),
            Err(s) => s,
        };
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn external_service_ignores_bearer_token_metadata() {
        // External listener uses extensions only — a bearer-token
        // header must NOT be consulted (defends against a refactor
        // that accidentally folds in extract_bearer_token).
        let state = make_state();
        let service =
            ControllerService::external(state, fixture_signing_key(), fixture_client_verifier());

        let req = authed(SubscribeRequest {}); // bearer token set, no extension
        let err = match service.subscribe(req).await {
            Ok(_) => panic!("bearer-token presence must not satisfy external strategy"),
            Err(s) => s,
        };
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn list_workspaces_returns_workspaces_for_verified_client() {
        use crate::signature_layer::VerifiedClient;
        use p256::ecdsa::SigningKey;
        use p256::elliptic_curve::rand_core::OsRng;
        use shared::client_signature::ClientRegistration;

        let state = make_state();
        let verifier = fixture_client_verifier();
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        verifier.registrations().write().await.insert(
            "client-alpha".to_string(),
            ClientRegistration {
                verifying_key: vk,
                workspaces: vec!["ws-a".into(), "ws-b".into()],
            },
        );
        let service = ControllerService::external(state, fixture_signing_key(), verifier.clone());

        let mut req = Request::new(ListWorkspacesRequest {});
        req.extensions_mut()
            .insert(VerifiedClient("client-alpha".to_string()));

        let resp = service.list_workspaces(req).await.unwrap().into_inner();
        assert_eq!(
            resp.workspaces,
            vec!["ws-a".to_string(), "ws-b".to_string()]
        );
    }

    #[tokio::test]
    async fn list_workspaces_returns_permission_denied_when_registration_evicted() {
        // The middleware stamped VerifiedClient(kid) but the cache no
        // longer holds that kid — operator deleted the Client CR in the
        // gap between middleware and handler. Fail closed.
        use crate::signature_layer::VerifiedClient;

        let state = make_state();
        let verifier = fixture_client_verifier();
        let service = ControllerService::external(state, fixture_signing_key(), verifier);

        let mut req = Request::new(ListWorkspacesRequest {});
        req.extensions_mut()
            .insert(VerifiedClient("client-evicted".to_string()));

        let err = service.list_workspaces(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn list_workspaces_rejects_missing_extension() {
        // No VerifiedClient on the request extensions → middleware
        // didn't run (or this is the internal listener). Fail closed.
        let state = make_state();
        let verifier = fixture_client_verifier();
        let service = ControllerService::external(state, fixture_signing_key(), verifier);

        let req = Request::new(ListWorkspacesRequest {});
        let err = service.list_workspaces(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn effective_history_limit_none_means_no_limit() {
        assert_eq!(effective_history_limit(None), None);
    }

    #[test]
    fn effective_history_limit_zero_means_no_limit() {
        // Proto convention: unset / 0 → no cap. Catches a regression
        // to treating `Some(0)` as "return zero entries."
        assert_eq!(effective_history_limit(Some(0)), None);
    }

    #[test]
    fn effective_history_limit_under_cap_passes_through() {
        assert_eq!(effective_history_limit(Some(42)), Some(42));
    }

    #[test]
    fn effective_history_limit_at_cap_passes_through() {
        assert_eq!(
            effective_history_limit(Some(MAX_HISTORY_LIMIT as u32)),
            Some(MAX_HISTORY_LIMIT),
        );
    }

    #[test]
    fn effective_history_limit_over_cap_clamps() {
        // Catches `<` vs `<=` and `min` vs `max` mutations on the clamp.
        assert_eq!(
            effective_history_limit(Some(MAX_HISTORY_LIMIT as u32 + 1)),
            Some(MAX_HISTORY_LIMIT),
        );
        assert_eq!(
            effective_history_limit(Some(u32::MAX)),
            Some(MAX_HISTORY_LIMIT),
        );
    }

    #[tokio::test]
    async fn set_conversation_name_handler_rejects_over_max_chars() {
        // The handler is the single server-side gate for
        // MAX_CONVERSATION_NAME_CHARS. Mutation target: flip the
        // comparison in the handler from `> MAX` to `> MAX + 1` — the
        // 201-char input slips through and `assert_eq!(InvalidArgument)`
        // below goes red. Verified that no other server-side layer
        // shadow-enforces the cap; storage and `WorkspaceState`
        // intentionally trust the handler.
        let state = make_state();
        let ws = state.get_or_create_workspace("default").await;
        let conv_id = ws.mint_conversation().await.unwrap();
        let service =
            ControllerService::internal(state, Some(fixed_pair("default")), fixture_signing_key());
        let err = service
            .set_conversation_name(authed(tightbeam_proto::SetConversationNameRequest {
                conversation_id: conv_id,
                name: "a".repeat(crate::conversation::MAX_CONVERSATION_NAME_CHARS + 1),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            err.message()
                .contains(&crate::conversation::MAX_CONVERSATION_NAME_CHARS.to_string()),
            "error must name the limit, got: {}",
            err.message()
        );
    }

    #[tokio::test]
    async fn get_conversation_history_rejects_empty_conversation_id() {
        let state = make_state();
        let service =
            ControllerService::internal(state, Some(fixed_pair("default")), fixture_signing_key());
        let err = service
            .get_conversation_history(authed(GetConversationHistoryRequest {
                conversation_id: String::new(),
                limit: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn get_conversation_history_returns_empty_for_fresh_conversation() {
        // Minted-but-untouched conversation: registry knows it, log is
        // empty. Snapshot returns empty entries with total_seq=0 and
        // truncated=false.
        let state = make_state();
        let ws = state.get_or_create_workspace("default").await;
        let conv_id = ws.mint_conversation().await.unwrap();
        let service = ControllerService::internal(
            state.clone(),
            Some(fixed_pair("default")),
            fixture_signing_key(),
        );
        let resp = service
            .get_conversation_history(authed(GetConversationHistoryRequest {
                conversation_id: conv_id,
                limit: None,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.entries.is_empty());
        assert_eq!(resp.total_seq, 0);
        assert!(!resp.truncated);
    }

    #[tokio::test]
    async fn get_conversation_history_returns_not_found_for_unminted_id() {
        // Speculative probe with a never-minted id must NOT silently
        // return empty-success — otherwise an attacker (or buggy client)
        // can enumerate ids cheaply and a deleted id would falsely appear
        // accessible. Workspace-prefix matches; registry-membership does not.
        let state = make_state();
        let service = ControllerService::internal(
            state.clone(),
            Some(fixed_pair("default")),
            fixture_signing_key(),
        );
        let err = service
            .get_conversation_history(authed(GetConversationHistoryRequest {
                conversation_id: "default.never-minted".into(),
                limit: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn get_conversation_history_returns_not_found_after_delete() {
        // End-to-end of the resurrection bug: mint, append, delete, then
        // a second device queries the same id. Handler must say NotFound,
        // not empty-success.
        use tightbeam_providers::types::{ContentBlock, Message};
        let state = make_state();
        let ws = state.get_or_create_workspace("default").await;
        let conv_id = ws.mint_conversation().await.unwrap();
        let log = ws.get_or_create_conversation(&conv_id).await.unwrap();
        log.write()
            .await
            .append(Message {
                role: "user".into(),
                content: Some(ContentBlock::text_content("hello")),
                tool_calls: None,
                tool_call_id: None,
                is_error: None,
            })
            .await
            .unwrap();
        ws.delete_conversation(&conv_id)
            .await
            .expect("delete succeeds");

        let service = ControllerService::internal(
            state.clone(),
            Some(fixed_pair("default")),
            fixture_signing_key(),
        );
        let err = service
            .get_conversation_history(authed(GetConversationHistoryRequest {
                conversation_id: conv_id,
                limit: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn get_conversation_history_without_verifier_returns_failed_precondition() {
        // Same auth gate every other authed RPC uses; pin it here so a
        // misrouted call to the unauthenticated-path test harness can't
        // sneak through.
        let state = make_state();
        let service = ControllerService::internal(state, None, fixture_signing_key());
        let err = service
            .get_conversation_history(authed(GetConversationHistoryRequest {
                conversation_id: "any".into(),
                limit: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn turn_with_reply_channel_propagates_to_pending() {
        let state = make_state();
        let service = ControllerService::internal(
            state.clone(),
            Some(fixed_pair("default")),
            fixture_signing_key(),
        );

        state
            .set_model_spec(
                "default".into(),
                crate::crd::ModelSpec {
                    provider_ref: crate::crd::ProviderRef {
                        name: "anthropic".into(),
                    },
                    model: "claude-sonnet-4-20250514".into(),
                    params: None,
                },
            )
            .await;
        state
            .set_provider_spec(
                "anthropic".into(),
                crate::crd::ProviderSpec {
                    format: "anthropic".into(),
                    base_url: Some("https://api.anthropic.com/v1".into()),
                    secret: crate::crd::ProviderSecret {
                        name: "anthropic-key".into(),
                        key: None,
                    },
                },
            )
            .await;
        state.set_job_connected("default", true).await;

        let state_clone = state.clone();
        let consumer =
            tokio::spawn(async move { state_clone.wait_for_turn("default").await.unwrap() });

        let request = authed(TurnRequest {
            system: Some("test".into()),
            tools: vec![],
            messages: vec![],
            model: None,
            reply_channel: Some("test-channel".into()),
            role: None,
            correlation_id: None,
            conversation_id: "default.test-conv".into(),
        });

        let result = service.turn(request).await;
        assert!(result.is_ok());

        let pending = consumer.await.unwrap();
        assert_eq!(
            pending.reply_channel.as_deref(),
            Some("test-channel"),
            "reply_channel must propagate from TurnRequest to PendingTurn"
        );
    }

    #[tokio::test]
    async fn params_json_none_when_neither_set() {
        let state = make_state();
        state
            .set_model_spec(
                "m".into(),
                crate::crd::ModelSpec {
                    provider_ref: crate::crd::ProviderRef {
                        name: "anthropic".into(),
                    },
                    model: "claude".into(),
                    params: None,
                },
            )
            .await;
        let result = build_params_json(&state, "m", None).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn params_json_carries_model_params_when_only_model_set() {
        let state = make_state();
        let mut params = serde_json::Map::new();
        params.insert("temperature".into(), serde_json::json!(0.7));
        state
            .set_model_spec(
                "m".into(),
                crate::crd::ModelSpec {
                    provider_ref: crate::crd::ProviderRef {
                        name: "anthropic".into(),
                    },
                    model: "claude".into(),
                    params: Some(params),
                },
            )
            .await;
        let result = build_params_json(&state, "m", None).await.expect("Some");
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed.get("temperature"), Some(&serde_json::json!(0.7)));
    }

    #[tokio::test]
    async fn params_json_merges_frontmatter_over_model_via_rfc7396() {
        let state = make_state();
        let mut model_params = serde_json::Map::new();
        model_params.insert("output_config".into(), serde_json::json!({"effort": "low"}));
        state
            .set_model_spec(
                "m".into(),
                crate::crd::ModelSpec {
                    provider_ref: crate::crd::ProviderRef {
                        name: "anthropic".into(),
                    },
                    model: "claude".into(),
                    params: Some(model_params),
                },
            )
            .await;

        let mut fm_params = serde_json::Map::new();
        fm_params.insert("output_config".into(), serde_json::json!({"effort": "max"}));

        let result = build_params_json(&state, "m", Some(&fm_params))
            .await
            .expect("Some");
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        // RFC 7396 recursive merge: frontmatter wins for `effort`.
        assert_eq!(
            parsed.get("output_config").and_then(|v| v.get("effort")),
            Some(&serde_json::json!("max"))
        );
    }

    // ---- channel_receive lifecycle tests ----
    //
    // These tests exercise the ChannelDropGuard contract directly: the
    // channel-registry entry must outlive any idle period (no timer-based
    // unregister) and must be cleared CHANNEL_DRAIN_SECS after the
    // outbound stream is dropped (registration tied to stream lifetime).

    /// Build the same guarded outbound stream that `channel_receive`
    /// returns to tonic. Returns the channel_id and the boxed stream so
    /// the test can hold or drop the stream as needed.
    #[allow(clippy::result_large_err)]
    async fn build_guarded_outbound(
        state: &Arc<ControllerState>,
    ) -> (
        String,
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChannelOutbound, Status>> + Send>>,
    ) {
        let (tx, rx) = mpsc::channel(16);
        let channel_id = state.mint_channel("ws-a".into(), None, tx).await;
        let guard = ChannelDropGuard {
            state: state.clone(),
            channel_id: channel_id.clone(),
        };
        let outbound_stream =
            ReceiverStream::new(rx).map(|msg| -> Result<ChannelOutbound, Status> { Ok(msg) });
        let guarded = GuardedStream {
            inner: outbound_stream,
            _guard: guard,
        };
        (channel_id, Box::pin(guarded))
    }

    #[tokio::test(start_paused = true)]
    async fn channel_receive_registration_survives_past_55s_idle() {
        let state = make_state();
        let (channel_id, _stream) = build_guarded_outbound(&state).await;

        // Past the old wall-clock unregister timer with no traffic.
        tokio::time::advance(std::time::Duration::from_secs(CHANNEL_DRAIN_SECS + 5)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            state.channel_workspace(&channel_id).await.as_deref(),
            Some("ws-a"),
            "channel must remain registered while the outbound stream is alive",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn channel_receive_unregister_fires_after_stream_drop_plus_drain() {
        let state = make_state();
        let (channel_id, stream) = build_guarded_outbound(&state).await;

        drop(stream);
        // Let the drop-guard's spawned task reach its sleep before we
        // start advancing the paused clock.
        tokio::task::yield_now().await;

        // Less than the drain delay — must still be registered.
        tokio::time::advance(std::time::Duration::from_secs(CHANNEL_DRAIN_SECS - 1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            state.channel_workspace(&channel_id).await.as_deref(),
            Some("ws-a"),
            "channel must stay registered during the drain window",
        );

        // Cross the drain boundary — must now be unregistered. Advance
        // past the deadline and yield enough times for the runtime to
        // wake the spawned drain task, run its post-sleep work, and let
        // the awaited unregister_channel land before we observe.
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert!(
            state.channel_workspace(&channel_id).await.is_none(),
            "channel must be unregistered after drain elapses",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn channel_receive_registration_present_during_drain_window() {
        // After the outbound stream drops, the registry entry must remain
        // intact through the entire drain window so any send_to_channel
        // call that races the drop still finds the entry. Walk several
        // checkpoints from t=0 to just under the drain boundary; the
        // lookup must succeed at every checkpoint.
        let state = make_state();
        let (channel_id, stream) = build_guarded_outbound(&state).await;

        drop(stream);

        let checkpoints = [1u64, 10, 30, CHANNEL_DRAIN_SECS - 1];
        let mut prev = 0u64;
        for target in checkpoints {
            let step = target - prev;
            tokio::time::advance(std::time::Duration::from_secs(step)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                state.channel_workspace(&channel_id).await.as_deref(),
                Some("ws-a"),
                "channel must remain registered throughout drain window (checkpoint={target}s)",
            );
            prev = target;
        }
    }
}
