use crate::state::{ActiveTurn, ControllerState, PendingTurn};
use futures::StreamExt;
use serde_json::Value;
use shared::auth::{extract_bearer_token, TokenVerifier};
use std::sync::Arc;

use hangar_proto::convert::chunk_to_turn_event;
use hangar_proto::hangar_controller_server::HangarController;
use hangar_proto::{
    turn_result_chunk, AwaitTurnCancelRequest, CancelTurnRequest, CancelTurnResponse,
    GetTurnRequest, TurnAck, TurnAssignment, TurnCancelSignal, TurnEvent, TurnRequest,
    TurnResultChunk, TurnRole,
};
use hangar_providers::merge::merge_rfc7396;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

/// Returns the request-body model name if it's a non-empty string, else None.
/// `params.model` is wire-optional but treats empty-string the same as absent.
fn non_empty_request_model(model: Option<&str>) -> Option<&str> {
    model.filter(|m| !m.is_empty())
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
    pub harness: Arc<dyn TokenVerifier>,
    pub llm: Arc<dyn TokenVerifier>,
}

/// Strategy for resolving the caller's workspace on the internal
/// listener. Constructed by the listener wiring in `main.rs`.
pub enum VerificationStrategy {
    /// Internal listener (port 9090): K8s SA token in `authorization`
    /// metadata, verified via TokenReview against one of the two
    /// audiences carried by the `RequiredAudience` extension.
    BearerToken(InternalVerifierPair),
    /// Misconfigured — no verifier wired. Every authenticated RPC
    /// fails with FailedPrecondition.
    None,
}

pub struct ControllerService {
    state: Arc<ControllerState>,
    strategy: VerificationStrategy,
}

impl ControllerService {
    /// Construct a controller service for the internal listener.
    /// `pair` carries the two TokenReview verifiers — one per audience.
    /// `None` means no kube client is available and the controller will
    /// reject all authed RPCs with FailedPrecondition.
    pub fn internal(state: Arc<ControllerState>, pair: Option<InternalVerifierPair>) -> Self {
        let strategy = match pair {
            Some(p) => VerificationStrategy::BearerToken(p),
            None => VerificationStrategy::None,
        };
        Self { state, strategy }
    }

    async fn verify_workspace<T>(&self, request: &Request<T>) -> Result<String, Status> {
        match &self.strategy {
            VerificationStrategy::BearerToken(pair) => {
                let token = extract_bearer_token(request)?;
                let verifier = pick_verifier(request, pair)?;
                verifier.verify_token(token).await
            }
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
        crate::audience_layer::RequiredAudience::Harness => Ok(&pair.harness),
        crate::audience_layer::RequiredAudience::Llm => Ok(&pair.llm),
    }
}

#[tonic::async_trait]
impl HangarController for ControllerService {
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
            .get("x-hangar-model")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| Status::invalid_argument("missing x-hangar-model metadata header"))?;

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

        drive_turn_result_stream(&self.state, stream, active, &model).await
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
                "TurnRequest.conversation_id must not be empty",
            ));
        }
        // conversation_id is an opaque token minted by the harness
        // (the per-workspace log author). Hangar treats it as passthrough
        // for routing/correlation; cross-workspace isolation is enforced by
        // `verify_workspace` + NetworkPolicy, not the id format.
        let conversation_id = params.conversation_id.clone();

        let role = params.role.and_then(|r| TurnRole::try_from(r).ok());

        // Model resolution: a non-empty `params.model`, else the reserved
        // `default` if registered, else the alphabetic-first model. The
        // harness strips frontmatter and resolves `model: inherit` before
        // dispatching, so `params.model` is already concrete and
        // `params.system` is dispatched as-is.
        let model = match non_empty_request_model(params.model.as_deref()) {
            Some(m) => m.to_string(),
            None => self
                .state
                .default_or_alphabetic_first()
                .await
                .ok_or_else(|| {
                    Status::failed_precondition(
                        "no model specified and no models registered: set `model` on TurnRequest, or register at least one Model",
                    )
                })?,
        };

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
                    return Err(Status::internal(format!("failed to create LLM Job: {e}")));
                }
                Err(_) => {
                    tracing::error!("turn: k8s API timed out creating Job (10s)");
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
                return Err(Status::deadline_exceeded(
                    "LLM Job did not connect within 30s",
                ));
            }
        }

        tracing::info!("turn: building assignment");

        let params_json = build_params_json(&self.state, &model, None).await;

        // hangar is stateless: the harness has already assembled the full
        // history into `params.messages` and stripped frontmatter from
        // `params.system`. Dispatch both as-is — no load, no append.
        let assignment = TurnAssignment {
            system: params.system.clone(),
            tools: params.tools,
            messages: params.messages,
            params_json,
            conversation_id: conversation_id.clone(),
        };

        // Register the per-turn cancel token keyed by (workspace,
        // conversation_id) before enqueue, so a CancelTurn that races the
        // llm-job's AwaitTurnCancel long-poll finds a token to fire.
        self.state
            .register_cancel(&workspace, &conversation_id)
            .await;

        let (result_tx, result_rx) = mpsc::channel(64);
        let pending = PendingTurn {
            assignment,
            result_tx,
            workspace: workspace.clone(),
            conversation_id: conversation_id.clone(),
            reply_channel: params.reply_channel,
            role,
            correlation_id: params.correlation_id,
            system_prompt: params.system,
        };

        tracing::info!(model = %model, "turn: enqueueing turn");
        // enqueue can fail before the turn is ever claimed via GetTurn; clean
        // up the token registered above so a failed enqueue cannot leak it.
        if let Err(e) = self.state.enqueue_turn(&model, pending).await {
            self.state.finish_turn(&workspace, &conversation_id).await;
            return Err(Status::internal(e));
        }
        tracing::info!("turn: enqueued, returning stream");

        #[allow(clippy::result_large_err)]
        let event_stream = ReceiverStream::new(result_rx)
            .map(|chunk| -> Result<TurnEvent, Status> { Ok(chunk_to_turn_event(chunk)) });

        Ok(Response::new(Box::pin(event_stream)))
    }

    async fn cancel_turn(
        &self,
        request: Request<CancelTurnRequest>,
    ) -> Result<Response<CancelTurnResponse>, Status> {
        // Resolve the caller's workspace from its SA token; the key is scoped by
        // it, never by the payload, so a cancel cannot fire another tenant's
        // turn.
        let workspace = self.verify_workspace(&request).await?;
        let conversation_id = request.into_inner().conversation_id;
        if conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "CancelTurnRequest.conversation_id must not be empty",
            ));
        }

        let cancelled = self.state.fire_cancel(&workspace, &conversation_id).await;
        tracing::info!(workspace = %workspace, conversation_id = %conversation_id, cancelled, "cancel turn requested");

        Ok(Response::new(CancelTurnResponse { cancelled }))
    }

    async fn await_turn_cancel(
        &self,
        request: Request<AwaitTurnCancelRequest>,
    ) -> Result<Response<TurnCancelSignal>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let conversation_id = request.into_inner().conversation_id;
        if conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "AwaitTurnCancelRequest.conversation_id must not be empty",
            ));
        }

        // Unknown/finished key: bare return, which the llm-job reads as "no
        // cancel". Otherwise block on a clone of the turn's token until a
        // CancelTurn fires it.
        if let Some(token) = self.state.cancel_token(&workspace, &conversation_id).await {
            token.cancelled().await;
        }

        Ok(Response::new(TurnCancelSignal {}))
    }
}

/// Per-chunk forward budget for the hand-off to the harness's Turn stream.
/// Kept ABOVE the harness's 45s idle-gap (turn.rs) so the controller defers
/// to the consumer's own timeout: a consumer that pauses then recovers within
/// its patience window still gets its reply, and one that genuinely gives up
/// drops its stream (making the next forward fail `Closed` immediately). This
/// only fires for a consumer that neither drains nor drops — with the worker's
/// own backstop behind it — instead of prematurely severing a slow-but-live
/// stream and discarding its terminal.
const FORWARD_GAP: std::time::Duration = std::time::Duration::from_secs(60);

/// Forward one chunk to the Turn caller, bounded by `FORWARD_GAP`. Returns
/// `false` when the consumer is gone (dropped, or not draining within the gap)
/// so the caller stops forwarding and just drains the worker stream to EOF —
/// which is what returns the keepalive worker to `GetTurn`.
async fn forward_chunk(active: &ActiveTurn, chunk: TurnResultChunk) -> bool {
    active
        .result_tx
        .sender()
        .send_timeout(chunk, FORWARD_GAP)
        .await
        .is_ok()
}

/// Drive the worker's `stream_turn_result` chunk stream: forward chunks to
/// the workspace (harness) as they arrive, deliver the user-facing reply
/// outbound, and surface a worker-reported `TurnError` to the client as
/// FAILED. hangar persists nothing — the harness owns conversation
/// history. Extracted from the handler so the loop/terminal logic is
/// unit-testable with a synthetic stream — tonic's `Streaming` cannot be
/// constructed in tests.
async fn drive_turn_result_stream<S>(
    state: &ControllerState,
    stream: S,
    mut active: ActiveTurn,
    model: &str,
) -> Result<Response<TurnAck>, Status>
where
    S: futures::Stream<Item = Result<TurnResultChunk, Status>>,
{
    // Run the drive body, then clean up on EVERY exit. A mid-stream worker
    // error returns early via `?` inside the body; routing all exits through
    // this single `finish_turn` before propagating the result means no exit
    // path can leak the per-turn cancel token.
    let result = drive_turn_result_body(state, stream, &mut active, model).await;
    state
        .finish_turn(&active.workspace, &active.conversation_id)
        .await;
    result
}

async fn drive_turn_result_body<S>(
    state: &ControllerState,
    stream: S,
    active: &mut ActiveTurn,
    model: &str,
) -> Result<Response<TurnAck>, Status>
where
    S: futures::Stream<Item = Result<TurnResultChunk, Status>>,
{
    futures::pin_mut!(stream);
    let mut complete_chunk: Option<TurnResultChunk> = None;
    let mut worker_error: Option<hangar_proto::TurnError> = None;
    // Once the harness stops draining its Turn stream (client disconnect,
    // stall, or an abandoned/backpressured stream), a blocking forward parks
    // forever on the bounded result channel and wedges the sole keepalive
    // worker. Instead we stop forwarding but keep reading the worker stream to
    // EOF, which is what returns the worker to GetTurn. See FORWARD_GAP.
    let mut downstream_alive = true;
    // Whether a terminal (Error or Complete) was actually delivered to the
    // consumer. Only then do we suppress the guard's fallback terminal — a
    // terminal that was produced but NOT delivered (consumer stalled) must
    // leave the guard to emit its fallback so the consumer is never left in
    // silence with a completed turn's reply discarded.
    let mut terminal_delivered = false;

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| Status::internal(format!("stream error: {e}")))?;
        // Buffer the terminal Complete so the reply lands before it; everything
        // else (deltas, worker errors) is forwarded immediately for streaming UX.
        if let Some(turn_result_chunk::Chunk::Complete(_)) = &chunk.chunk {
            complete_chunk = Some(chunk);
            continue;
        }
        let is_error = matches!(&chunk.chunk, Some(turn_result_chunk::Chunk::Error(_)));
        if let Some(turn_result_chunk::Chunk::Error(e)) = &chunk.chunk {
            // Capture so we broadcast FAILED after the stream ends. The worker
            // stays alive and loops back to GetTurn — per-turn failure, not a
            // teardown.
            worker_error = Some(e.clone());
        }
        if downstream_alive {
            if forward_chunk(active, chunk).await {
                if is_error {
                    terminal_delivered = true;
                }
            } else {
                downstream_alive = false;
                tracing::warn!(
                    workspace = %active.workspace,
                    conversation_id = %active.conversation_id,
                    "turn consumer stopped draining; draining worker to EOF to free the keepalive job",
                );
            }
        }
    }

    // Whether a produced terminal (Error or Complete) failed to reach the
    // consumer. A worker-reported error wins over any buffered Complete (tell
    // the client FAILED, not a reply); an empty stream produced no terminal at
    // all, so nothing is stranded.
    let stranded = if let Some(err) = worker_error {
        tracing::warn!(
            workspace = %active.workspace,
            conversation_id = %active.conversation_id,
            code = err.code,
            error = %err.message,
            "turn failed: worker reported an error",
        );
        // The error terminal was forwarded in the loop; stranded iff it never
        // reached the consumer (which had already stalled/gone).
        !terminal_delivered
    } else if let Some(complete_chunk) = complete_chunk {
        let delivered = downstream_alive && forward_chunk(active, complete_chunk).await;
        // Keepalive: bump on a completed turn so the idle sweep doesn't reap
        // a Job that just did useful work.
        state.bump_model_activity(model).await;
        !delivered
    } else {
        false
    };
    // Suppress the guard's fallback terminal unless a produced terminal was
    // stranded — then leave the guard to emit a fallback so a completed/failed
    // turn is never silently dropped on a consumer that stalled.
    if !stranded {
        active.result_tx.mark_complete();
    }
    Ok(Response::new(TurnAck {}))
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
            harness: fixed_verifier(name),
            llm: fixed_verifier(name),
        }
    }

    /// Tonic Request<T> stamped with the harness audience extension
    /// (matching what the `audience_layer` would do in production). All
    /// non-LLM RPCs go through this helper.
    fn authed<T>(inner: T) -> Request<T> {
        let mut req = Request::new(inner);
        req.metadata_mut()
            .insert("authorization", "Bearer test".parse().unwrap());
        req.extensions_mut()
            .insert(crate::audience_layer::RequiredAudience::Harness);
        req
    }

    fn make_state() -> Arc<ControllerState> {
        Arc::new(ControllerState::new(
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
            shared::scheduling::SchedulingConfig::default(),
        ))
    }

    // ---- drive_turn_result_stream: worker-reported failures reach the client ----

    fn content_delta(text: &str) -> TurnResultChunk {
        TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::ContentDelta(
                hangar_proto::ContentDelta { text: text.into() },
            )),
        }
    }

    fn worker_error_chunk(code: i32, message: &str) -> TurnResultChunk {
        TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Error(hangar_proto::TurnError {
                code,
                message: message.into(),
            })),
        }
    }

    fn terminal_complete() -> TurnResultChunk {
        TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Complete(
                hangar_proto::TurnComplete {
                    stop_reason: 0,
                    content: vec![],
                    tool_calls: vec![],
                },
            )),
        }
    }

    fn active_turn_with(
        reply_channel: Option<String>,
        result_tx: mpsc::Sender<TurnResultChunk>,
    ) -> ActiveTurn {
        ActiveTurn {
            result_tx: crate::state::TurnResultGuard::new(result_tx),
            workspace: "ws".into(),
            conversation_id: "ws.c".into(),
            reply_channel,
            role: None,
            correlation_id: None,
            system_prompt: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_consumer_frees_worker_instead_of_wedging() {
        // THE FREEZE regression. A Turn caller that holds its result stream
        // open but stops draining it must NOT wedge the single keepalive
        // worker. drive_turn_result_stream must stop forwarding after
        // FORWARD_GAP, drain the worker stream to EOF, and return Ok so the
        // worker loops back to GetTurn. `start_paused` auto-advances the
        // virtual clock past FORWARD_GAP instantly, so the test is fast.
        // Mutant: revert the forward to a blocking `send().await` → forwarding
        // parks forever, the outer virtual timeout trips, and this goes red.
        let state = make_state();
        // Small buffer + a receiver we deliberately never drain and never drop
        // (the live-but-stalled consumer that caused the outage).
        let (result_tx, _result_rx) = mpsc::channel::<TurnResultChunk>(2);
        let active = active_turn_with(None, result_tx);

        // More chunks than the buffer holds, so forwarding must block then time
        // out, flipping downstream_alive and draining the rest.
        let stream =
            futures::stream::iter((0..10).map(|i| content_delta(&format!("c{i}")))).map(Ok);

        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            drive_turn_result_stream(&state, stream, active, "m"),
        )
        .await;
        assert!(
            matches!(resp, Ok(Ok(_))),
            "a stalled-but-alive consumer must free the worker (Ok), not wedge it: {resp:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_consumer_with_buffered_complete_is_not_left_silent() {
        // A completed turn (buffered Complete) whose consumer stalled must not be
        // silently swallowed: drive returns Ok (frees the worker) AND the
        // consumer's stream TERMINATES (yields buffered items then closes) rather
        // than hanging. cap=1 makes the first delta fill the buffer so the
        // buffered Complete's send times out (start_paused advances past
        // FORWARD_GAP instantly). Mutant: if a blocking send replaced
        // send_timeout, drive would hang and the outer timeout would trip.
        let state = make_state();
        let (result_tx, mut result_rx) = mpsc::channel::<TurnResultChunk>(1);
        let active = active_turn_with(None, result_tx);

        let stream = futures::stream::iter(vec![Ok(content_delta("a")), Ok(terminal_complete())]);
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            drive_turn_result_stream(&state, stream, active, "m"),
        )
        .await;
        assert!(matches!(resp, Ok(Ok(_))), "worker must be freed: {resp:?}");

        // The consumer's stream must terminate (buffered delta, then close),
        // never hang. drive owns the guard, so its drop closes the sender.
        assert!(result_rx.recv().await.is_some(), "buffered delta delivered");
        let mut drained = false;
        while let Ok(item) =
            tokio::time::timeout(std::time::Duration::from_secs(5), result_rx.recv()).await
        {
            match item {
                Some(_) => {}
                None => {
                    drained = true;
                    break;
                }
            }
        }
        assert!(
            drained,
            "stream must close (None), not leave the consumer silent"
        );
    }

    #[tokio::test]
    async fn worker_error_chunk_yields_clean_ack() {
        // A worker TurnError is still a clean ACK to the worker (it loops
        // back to GetTurn). The client learns of the failure via the
        // gateway's DeliverOutbound (covered by the gateway tests); the
        // FAILED decision itself is covered by `provider_error_turn_state`.
        // Mutant: propagate the error as Err → this goes red.
        let state = make_state();
        let (result_tx, _result_rx) = mpsc::channel::<TurnResultChunk>(64);
        let active = active_turn_with(Some("ch".into()), result_tx);

        let stream = futures::stream::iter(vec![
            Ok(content_delta("partial")),
            Ok(worker_error_chunk(
                -1,
                "API error 400: credit balance too low",
            )),
        ]);
        let resp = drive_turn_result_stream(&state, stream, active, "m").await;
        assert!(
            resp.is_ok(),
            "a reported error is still a clean ACK to the worker"
        );
    }

    #[tokio::test]
    async fn worker_error_chunk_forwarded_with_single_terminal() {
        // The error chunk is still forwarded to the workspace (so the agent
        // loop unblocks), and mark_complete() stops the guard appending a
        // SECOND terminal. Mutant: drop the forward → harness hangs; drop
        // mark_complete() → a spurious second Unavailable chunk appears.
        let state = make_state();
        let (result_tx, mut result_rx) = mpsc::channel::<TurnResultChunk>(64);
        let active = active_turn_with(Some("ch".into()), result_tx);

        let stream = futures::stream::iter(vec![Ok(worker_error_chunk(-1, "boom"))]);
        drive_turn_result_stream(&state, stream, active, "m")
            .await
            .unwrap();

        let first = result_rx.recv().await.expect("error chunk forwarded");
        assert!(
            matches!(first.chunk, Some(turn_result_chunk::Chunk::Error(_))),
            "the worker error is forwarded to the workspace stream",
        );
        assert!(
            result_rx.recv().await.is_none(),
            "exactly one terminal — the guard must not append a second",
        );
    }

    #[tokio::test]
    async fn mid_stream_error_still_finishes_cancel_token() {
        // a worker-stream transport error mid-drive returns Err via
        // `?`, and MUST still run finish_turn so the per-turn cancel token
        // cannot leak. `active_turn_with` uses workspace "ws" / conversation
        // "ws.c"; register a token under that key, drive a stream that errors
        // mid-way, then assert the token is gone. Mutant: propagate the
        // mid-stream `?` before cleanup (the pre-fix shape) → the token
        // survives and this goes red.
        let state = make_state();
        state.register_cancel("ws", "ws.c").await;
        assert!(
            state.cancel_token("ws", "ws.c").await.is_some(),
            "precondition: token registered",
        );
        let (result_tx, _result_rx) = mpsc::channel::<TurnResultChunk>(64);
        let active = active_turn_with(None, result_tx);

        let stream = futures::stream::iter(vec![
            Ok(content_delta("partial")),
            Err(tonic::Status::internal("boom")),
        ]);
        let resp = drive_turn_result_stream(&state, stream, active, "m").await;
        assert!(
            resp.is_err(),
            "a mid-stream worker error must surface as Err"
        );
        assert!(
            state.cancel_token("ws", "ws.c").await.is_none(),
            "finish_turn must run on the error path so the token cannot leak",
        );
    }

    #[tokio::test]
    async fn empty_stream_does_not_forward_error_terminal() {
        // No chunks and no error → "streamed nothing", a clean ACK. The
        // result channel closes with NO error terminal forwarded: the
        // worker-error path must not fire when there was no error. Mutant:
        // forward a FAILED/error chunk unconditionally → recv sees an Error.
        let state = make_state();
        let (result_tx, mut result_rx) = mpsc::channel::<TurnResultChunk>(64);
        let active = active_turn_with(Some("ch".into()), result_tx);

        let stream = futures::stream::iter(Vec::<Result<TurnResultChunk, tonic::Status>>::new());
        drive_turn_result_stream(&state, stream, active, "m")
            .await
            .unwrap();

        assert!(
            result_rx.recv().await.is_none(),
            "no error → clean close, no error terminal forwarded",
        );
    }

    // -- Pure helpers extracted from handler boundaries. Kept tested
    // -- separately so cargo-mutants can prove every branch is reachable.

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

    #[tokio::test]
    async fn turn_errors_when_no_verifier_configured() {
        // Replaces the old `turn_without_verifier_uses_default_workspace`
        // test, whose premise (silent fallback to workspace="default") was
        // the reserved-name anti-pattern this change deletes.
        let state = make_state();
        let service = ControllerService::internal(state.clone(), None);

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
    async fn turn_with_reply_channel_propagates_to_pending() {
        let state = make_state();
        let service = ControllerService::internal(state.clone(), Some(fixed_pair("default")));

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
}
