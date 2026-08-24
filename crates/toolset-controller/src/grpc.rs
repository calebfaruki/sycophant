use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tracing::info;
use uuid::Uuid;

use toolset_proto::convert::chunk_to_turn_event;
use toolset_proto::toolset_controller_server::ToolsetController;
use toolset_proto::{
    turn_result_chunk, AwaitToolCancelRequest, AwaitTurnCancelRequest, CancelToolCallRequest,
    CancelToolCallResponse, GetToolCallRequest, GetTurnRequest, ReportDiscoveredToolsAck,
    ReportDiscoveredToolsRequest, SendToolResultAck, ToolCallAssignment, ToolCallHandle,
    ToolCancelSignal, TurnAck, TurnAssignment, TurnCancelSignal, TurnEvent, TurnRequest,
    TurnResultChunk, TurnRole,
};

use proto_common::tool_result_frame::Frame;
use proto_common::{
    AwaitToolResultRequest, CallToolRequest, CancelTurnRequest, CancelTurnResponse, ToolInfo,
    ToolListUpdate, ToolResultFrame, WatchToolsRequest,
};

use crate::audience_layer::RequiredAudience;
use crate::job;
use crate::keepalive::TOOL_KEEPALIVE_IDLE_SECONDS;
use crate::registry::{ArgDecl, ArgType};
use crate::state::{
    ActiveJob, ActiveTurn, ControllerState, Grant, PendingCall, PendingTurn, PromptConfig,
    PromptReady, RegisteredTool, WorkspaceBindings, RESULT_CHANNEL_CAPACITY,
};
use crate::validation::{synthesize_schema, validate_call_input};
use crate::WORKSPACE_MOUNT_PATH;
use shared::auth::{extract_bearer_token, TokenVerifier};
use shared::keepalive::{delete_job, job_health, JobHealth, STARTUP_GRACE};

/// Delete a Job, warning on failure instead of surfacing it: every caller is
/// already past the point of acting on the error, but a lingering pod must
/// leave a trace for the operator.
async fn delete_job_logged(client: &kube::Client, namespace: &str, job_name: &str) {
    if let Err(e) = delete_job(client, namespace, job_name).await {
        tracing::warn!(job = %job_name, error = %e, "Job delete failed; pod may linger");
    }
}

/// Pair of TokenReview verifiers for the single gRPC listener — one per
/// audience. The `audience_layer` middleware stamps a [`RequiredAudience`]
/// extension on each request from its method path; the handler picks the
/// matching verifier here.
pub struct VerifierPair {
    pub harness: Arc<dyn TokenVerifier>,
    pub tool_job: Arc<dyn TokenVerifier>,
}

pub struct ControllerService {
    state: Arc<ControllerState>,
    verifiers: Option<VerifierPair>,
    bindings: WorkspaceBindings,
    prompt: PromptConfig,
}

impl ControllerService {
    pub fn new(
        state: Arc<ControllerState>,
        verifiers: Option<VerifierPair>,
        bindings: WorkspaceBindings,
        prompt: PromptConfig,
    ) -> Self {
        Self {
            state,
            verifiers,
            bindings,
            prompt,
        }
    }

    /// Resolve the caller's workspace, tolerating an unconfigured verifier (the
    /// no-auth development/test path returns `None`). Used by the tool-dispatch
    /// surface, whose binding check is skipped when identity is unknown.
    async fn verify_workspace_optional<T>(
        &self,
        request: &Request<T>,
    ) -> Result<Option<String>, Status> {
        match &self.verifiers {
            Some(pair) => {
                let token = extract_bearer_token(request)?;
                let verifier = pick_verifier(request, pair)?;
                Ok(Some(verifier.verify_token(token).await?))
            }
            None => Ok(None),
        }
    }

    /// Resolve the caller's workspace, failing closed when no verifier is
    /// configured. Used by the turn-dispatch surface, which enforces
    /// per-workspace turn ownership and so cannot proceed without an identity.
    async fn verify_workspace_required<T>(&self, request: &Request<T>) -> Result<String, Status> {
        match &self.verifiers {
            Some(pair) => {
                let token = extract_bearer_token(request)?;
                let verifier = pick_verifier(request, pair)?;
                verifier.verify_token(token).await
            }
            None => Err(Status::failed_precondition(
                "no token verifier configured: workspace identity cannot be established",
            )),
        }
    }

    /// Whether `caller` owns the tool call `call_id`. An unknown call id and a
    /// non-owner are indistinguishable to the caller: both are `false`, and each
    /// handler answers with its own unknown-call-id outcome, so a non-owner
    /// never learns that the call exists.
    async fn caller_owns_call(&self, caller: &str, call_id: &str, rpc: &'static str) -> bool {
        match self.state.call_owner(call_id).await {
            Some(owner) if owner == caller => true,
            Some(owner) => {
                tracing::warn!(
                    rpc,
                    caller_workspace = caller,
                    attempted_owner = %owner,
                    "cross-workspace tool-call access denied",
                );
                false
            }
            None => false,
        }
    }
}

/// Pick the verifier matching the request's `RequiredAudience` extension. The
/// audience layer must have run; otherwise the request fails closed with
/// `Internal("audience layer not wired")`.
#[allow(clippy::result_large_err)]
fn pick_verifier<'a, T>(
    request: &Request<T>,
    pair: &'a VerifierPair,
) -> Result<&'a Arc<dyn TokenVerifier>, Status> {
    let required = request
        .extensions()
        .get::<RequiredAudience>()
        .ok_or_else(|| {
            Status::internal(
                "audience layer not wired: the listener must install RequiredAudienceLayer",
            )
        })?;
    match required {
        RequiredAudience::Harness => Ok(&pair.harness),
        RequiredAudience::ToolJob => Ok(&pair.tool_job),
    }
}

/// Enforce that the verified caller owns the turn under operation.
///
/// Returns `NotFound` on mismatch (not `PermissionDenied`) to avoid leaking the
/// existence of cross-workspace turn IDs (OWASP API1:2023 BOLA). The denial
/// reason is captured in the warn-level structured log.
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

/// Returns the request-body model name if it's a non-empty string, else None.
fn non_empty_request_model(model: Option<&str>) -> Option<&str> {
    model.filter(|m| !m.is_empty())
}

/// A grant a call selected: the name as stored in the binding, and the grant.
type ResolvedGrant<'a> = (&'a str, &'a Grant);

/// Split a tool call's input into the JSON its declared arguments are validated
/// against and the grant the reserved `__grant` key selects.
///
/// `__grant` is compared as an exact member of the menu bound for this
/// (workspace, toolset) pair — never trimmed, case-folded, normalized, or
/// resolved as a path — and is removed before validation, which admits only
/// declared arguments. Absence of the key is the grantless path, not a miss.
#[allow(clippy::result_large_err)] // tonic::Status is the gRPC-shaped error this layer returns
fn take_grant<'a>(
    input_json: &str,
    bindings: &'a WorkspaceBindings,
    workspace: &str,
    toolset: &str,
) -> Result<(String, Option<ResolvedGrant<'a>>), Status> {
    // A payload that is not a JSON object carries no key to remove; input
    // validation reports the shape error itself.
    let Ok(serde_json::Value::Object(mut input)) = serde_json::from_str(input_json) else {
        return Ok((input_json.to_string(), None));
    };
    let Some(selected) = input.remove("__grant") else {
        return Ok((input_json.to_string(), None));
    };
    let serde_json::Value::String(name) = selected else {
        return Err(Status::permission_denied(
            "__grant must name one grant as a string",
        ));
    };
    let resolved = bindings
        .grants_for(workspace, toolset)
        .and_then(|menu| menu.get_key_value(name.as_str()))
        .map(|(bound, grant)| (bound.as_str(), grant))
        .ok_or_else(|| {
            Status::permission_denied(format!(
                "grant '{name}' is not bound for workspace {workspace} on toolset {toolset}"
            ))
        })?;
    Ok((serde_json::Value::Object(input).to_string(), Some(resolved)))
}

async fn snapshot_tools_for(
    state: &ControllerState,
    workspace: Option<&str>,
    bindings: &WorkspaceBindings,
) -> Vec<ToolInfo> {
    let raw = match workspace {
        Some(ws) => state.list_tools_for_workspace(ws, bindings).await,
        None => state.list_tools().await,
    };
    raw.into_iter()
        .map(|(name, tool)| ToolInfo {
            name,
            description: tool.description,
            parameters_json: synthesize_schema(&tool.args),
        })
        .collect()
}

#[tonic::async_trait]
impl ToolsetController for ControllerService {
    // =====================================================================
    // Turn dispatch
    // =====================================================================

    type TurnStream = Pin<Box<dyn Stream<Item = Result<TurnEvent, Status>> + Send + 'static>>;

    async fn turn(
        &self,
        request: Request<TurnRequest>,
    ) -> Result<Response<Self::TurnStream>, Status> {
        let workspace = self.verify_workspace_required(&request).await?;
        let params = request.into_inner();

        if params.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "TurnRequest.conversation_id must not be empty",
            ));
        }
        let conversation_id = params.conversation_id.clone();
        let role = params.role.and_then(|r| TurnRole::try_from(r).ok());

        // Profile resolution: the turn's `model` value names a profile of the
        // prompt configuration section. Fail-closed — an absent key is refused,
        // never routed to a default or an arbitrary other profile.
        let model = non_empty_request_model(params.model.as_deref())
            .ok_or_else(|| {
                Status::failed_precondition(
                    "TurnRequest.model must name a prompt profile: refusing the turn (no default)",
                )
            })?
            .to_string();

        let profile = self.prompt.get(&model).cloned().ok_or_else(|| {
            Status::failed_precondition(format!(
                "no prompt profile '{model}': refusing the turn (no fallback)"
            ))
        })?;

        self.state.ensure_model_slot(&model).await;

        if self.state.is_job_connected(&model).await {
            tracing::debug!(model = %model, "reusing existing prompt job");
        } else if let Some(client) = self.state.kube_client() {
            let addr = self.state.controller_addr().to_owned();
            let ns = self.state.namespace().to_owned();

            tracing::info!(model = %model, "turn: no prompt job connected, creating one");
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                job::create_prompt_job(
                    client,
                    &model,
                    &profile,
                    &addr,
                    &ns,
                    &workspace,
                    self.state.scheduling(),
                ),
            )
            .await
            {
                Ok(Ok(name)) => {
                    tracing::info!(job = %name, "turn: prompt Job created");
                    self.state.set_prompt_job_launching(&model, name).await;
                    self.state.bump_model_activity(&model).await;
                }
                Ok(Err(e)) => {
                    tracing::error!("turn: k8s API rejected Job creation: {e}");
                    return Err(Status::internal(format!(
                        "failed to create prompt Job: {e}"
                    )));
                }
                Err(_) => {
                    tracing::error!("turn: k8s API timed out creating Job (10s)");
                    return Err(Status::internal("k8s API timed out creating prompt Job"));
                }
            }

            // Ready means the job asked for work. On expiry delete the job and
            // leave the slot idle, so the next turn launches a fresh one
            // instead of silently reusing a zombie.
            if let PromptReady::Expired { job_name } =
                self.state.wait_for_job_connect(&model, READY_TIMEOUT).await
            {
                tracing::warn!(
                    model = %model,
                    job = job_name.as_deref().unwrap_or("<none>"),
                    "turn: prompt Job did not ask for work within readyTimeout; deleting it"
                );
                if let Some(job_name) = job_name {
                    delete_job_logged(client, self.state.namespace(), &job_name).await;
                }
                return Err(Status::deadline_exceeded(
                    "prompt Job did not ask for work within readyTimeout",
                ));
            }
        } else {
            tracing::error!(model = %model, "no kube client at request time");
        }

        // The controller is stateless: the harness has already assembled the
        // full history into `params.messages` and stripped frontmatter from
        // `params.system`. Dispatch both as-is.
        let assignment = TurnAssignment {
            system: params.system.clone(),
            tools: params.tools,
            messages: params.messages,
            conversation_id: conversation_id.clone(),
        };

        // Register the per-turn cancel token before enqueue so a CancelTurn
        // that races the prompt job's AwaitTurnCancel long-poll finds a token.
        self.state
            .register_cancel(&workspace, &conversation_id)
            .await;

        let (result_tx, result_rx) = mpsc::channel(RESULT_CHANNEL_CAPACITY);
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

        if let Err(e) = self.state.enqueue_turn(&model, pending).await {
            self.state.finish_turn(&workspace, &conversation_id).await;
            return Err(Status::internal(e));
        }

        #[allow(clippy::result_large_err)]
        let event_stream = ReceiverStream::new(result_rx)
            .map(|chunk| -> Result<TurnEvent, Status> { Ok(chunk_to_turn_event(chunk)) });

        Ok(Response::new(Box::pin(event_stream)))
    }

    async fn cancel_turn(
        &self,
        request: Request<CancelTurnRequest>,
    ) -> Result<Response<CancelTurnResponse>, Status> {
        let workspace = self.verify_workspace_required(&request).await?;
        let conversation_id = request.into_inner().conversation_id;
        if conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "CancelTurnRequest.conversation_id must not be empty",
            ));
        }

        let cancelled = self.state.fire_cancel(&workspace, &conversation_id).await;
        info!(workspace = %workspace, conversation_id = %conversation_id, cancelled, "cancel turn requested");

        Ok(Response::new(CancelTurnResponse { cancelled }))
    }

    async fn get_turn(
        &self,
        request: Request<GetTurnRequest>,
    ) -> Result<Response<TurnAssignment>, Status> {
        // The prompt-job pod runs with sa-<workspace>, so its identity binds
        // to a specific workspace; verify the dequeued turn's workspace matches.
        let caller_workspace = self.verify_workspace_required(&request).await?;
        let req = request.into_inner();
        if req.model_name.is_empty() {
            return Err(Status::invalid_argument(
                "GetTurnRequest.model_name must be set: the prompt job must declare which model it serves",
            ));
        }
        let model = req.model_name;

        // A job whose readyTimeout already fired cannot re-register itself: the
        // slot is idle and the turn that launched it is gone.
        if !self.state.connect_prompt_job(&model).await {
            return Err(Status::failed_precondition(format!(
                "no prompt job launching for profile '{model}'"
            )));
        }
        self.state.bump_model_activity(&model).await;

        let pending = self
            .state
            .wait_for_turn(&model)
            .await
            .ok_or_else(|| Status::unavailable("controller shutting down"))?;

        enforce_caller_owns_turn(&caller_workspace, &pending.workspace, "get_turn")?;

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
        // Request<Streaming<_>> is not Sync, so decompose first then synthesize
        // a Request<()> carrying the metadata + extensions for verification.
        let (metadata, extensions, stream) = {
            let metadata = request.metadata().clone();
            let extensions = request.extensions().clone();
            let stream = request.into_inner();
            (metadata, extensions, stream)
        };
        let mut auth_request = Request::new(());
        *auth_request.metadata_mut() = metadata.clone();
        *auth_request.extensions_mut() = extensions;
        let caller_workspace = self.verify_workspace_required(&auth_request).await?;

        let model = metadata
            .get("x-toolset-model")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| Status::invalid_argument("missing x-toolset-model metadata header"))?;

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

    async fn await_turn_cancel(
        &self,
        request: Request<AwaitTurnCancelRequest>,
    ) -> Result<Response<TurnCancelSignal>, Status> {
        let workspace = self.verify_workspace_required(&request).await?;
        let conversation_id = request.into_inner().conversation_id;
        if conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "AwaitTurnCancelRequest.conversation_id must not be empty",
            ));
        }

        if let Some(token) = self.state.cancel_token(&workspace, &conversation_id).await {
            token.cancelled().await;
        }

        Ok(Response::new(TurnCancelSignal {}))
    }

    // =====================================================================
    // Tool dispatch
    // =====================================================================

    type WatchToolsStream =
        Pin<Box<dyn Stream<Item = Result<ToolListUpdate, Status>> + Send + 'static>>;

    type AwaitToolResultStream =
        Pin<Box<dyn Stream<Item = Result<ToolResultFrame, Status>> + Send + 'static>>;

    async fn watch_tools(
        &self,
        request: Request<WatchToolsRequest>,
    ) -> Result<Response<Self::WatchToolsStream>, Status> {
        let workspace = self.verify_workspace_optional(&request).await?;

        let state = self.state.clone();
        let bindings = self.bindings.clone();
        let mut rev_rx = state.subscribe_tools_revision();
        let (tx, rx) = mpsc::channel::<Result<ToolListUpdate, Status>>(8);

        tokio::spawn(async move {
            loop {
                let tools = snapshot_tools_for(&state, workspace.as_deref(), &bindings).await;
                if tx.send(Ok(ToolListUpdate { tools })).await.is_err() {
                    break; // client disconnected
                }
                if rev_rx.changed().await.is_err() {
                    break; // state's sender dropped (process shutting down)
                }
            }
        });

        let stream: Self::WatchToolsStream = Box::pin(ReceiverStream::new(rx));
        Ok(Response::new(stream))
    }

    async fn begin_tool_call(
        &self,
        request: Request<CallToolRequest>,
    ) -> Result<Response<ToolCallHandle>, Status> {
        let workspace = self.verify_workspace_required(&request).await?;

        let req = request.into_inner();
        let tool_name = &req.name;

        let tool = self
            .state
            .get_tool(tool_name)
            .await
            .ok_or_else(|| Status::not_found(format!("unknown tool: {tool_name}")))?;

        if !self.bindings.has_toolset(&workspace, &tool.toolset_name) {
            return Err(Status::permission_denied(format!(
                "workspace {workspace} is not authorized for toolset {}",
                tool.toolset_name
            )));
        }

        let (input_json, grant) = take_grant(
            &req.input_json,
            &self.bindings,
            &workspace,
            &tool.toolset_name,
        )?;

        let args = validate_call_input(&input_json, &tool.args)?;

        let entry = self
            .state
            .get_toolset(&tool.toolset_name)
            .await
            .ok_or_else(|| {
                Status::failed_precondition(format!("toolset {} not found", tool.toolset_name))
            })?;

        let call_id = Uuid::new_v4().to_string();
        let working_dir = WORKSPACE_MOUNT_PATH.to_string();

        // Whether this call must be bounded by a ready deadline. It must when it
        // spawns a job, and when it attaches to one that has not yet started
        // running. A job that is already ready needs no deadline, and running
        // work carries no time bound at all.
        let mut needs_deadline = false;

        // Per-tool dispatch mutex held only across the get-probe-create-set
        // sequence so concurrent calls for the same tool cannot both spawn.
        {
            let dispatch_lock = self.state.tool_dispatch_lock(&workspace, tool_name).await;
            let _dispatch_guard = dispatch_lock.lock().await;

            if let Some(client) = self.state.kube_client() {
                let workspace_pvc = format!("{}-workspace-data", workspace);
                needs_deadline = true;

                let should_spawn = match self.state.get_active_job(&workspace, tool_name).await {
                    None => true,
                    // A record naming no job id was adopted at reconcile: the
                    // controller cannot match a GetToolCall against it, so it is
                    // not attachable and takes the delete-and-respawn branch.
                    Some(active) if active.job_id.is_empty() => {
                        info!(
                            tool = %tool_name,
                            workspace = %workspace,
                            adopted_job = %active.job_name,
                            "adopted ActiveJob names no call id; deleting + recreating"
                        );
                        self.state.remove_active_job(&workspace, tool_name).await;
                        delete_job_logged(client, self.state.namespace(), &active.job_name).await;
                        true
                    }
                    Some(active) => {
                        let health =
                            job_health(client, self.state.namespace(), &active.job_name).await;
                        match health {
                            JobHealth::Running => {
                                needs_deadline = false;
                                false
                            }
                            JobHealth::Pending { age } if age < STARTUP_GRACE => false,
                            JobHealth::Pending { .. } | JobHealth::Failed | JobHealth::NotFound => {
                                info!(
                                    tool = %tool_name,
                                    workspace = %workspace,
                                    stale_job = %active.job_name,
                                    health = ?health,
                                    "stale ActiveJob entry; deleting + recreating"
                                );
                                self.state.remove_active_job(&workspace, tool_name).await;
                                delete_job_logged(client, self.state.namespace(), &active.job_name)
                                    .await;
                                true
                            }
                        }
                    }
                };

                if should_spawn {
                    let job_spec = job::build_tool_job(
                        tool_name,
                        &tool.toolset_name,
                        &entry,
                        &call_id,
                        self.state.namespace(),
                        self.state.controller_addr(),
                        &workspace,
                        &workspace_pvc,
                        self.state.scheduling(),
                        grant,
                    );
                    let job_name = job_spec
                        .metadata
                        .name
                        .clone()
                        .expect("build_tool_job always sets metadata.name");
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        job::create_job(client, self.state.namespace(), &job_spec),
                    )
                    .await
                    {
                        Ok(Ok(_)) => {
                            info!(call_id = %call_id, tool = %tool_name, "tool Job created");
                        }
                        Ok(Err(e)) => {
                            tracing::error!(
                                call_id = %call_id,
                                "k8s API rejected tool Job creation: {e}"
                            );
                            return Err(Status::internal(format!(
                                "failed to create tool Job: {e}"
                            )));
                        }
                        Err(_) => {
                            tracing::error!(
                                call_id = %call_id,
                                "k8s API timed out creating tool Job (10s)"
                            );
                            return Err(Status::internal("k8s API timed out creating tool Job"));
                        }
                    }
                    self.state
                        .set_active_job(ActiveJob {
                            job_name,
                            job_id: call_id.clone(),
                            tool_name: tool_name.clone(),
                            workspace: workspace.clone(),
                            last_activity: std::time::Instant::now(),
                            keepalive_seconds: if entry.keepalive {
                                TOOL_KEEPALIVE_IDLE_SECONDS
                            } else {
                                0
                            },
                        })
                        .await;
                }
            }
        }

        let (result_tx, result_rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);

        self.state
            .set_result_tx(
                call_id.clone(),
                workspace.clone(),
                tool_name.clone(),
                result_tx,
            )
            .await;
        self.state.set_result_rx(call_id.clone(), result_rx).await;
        self.state.register_call_cancel(call_id.clone()).await;

        self.state
            .enqueue_call(PendingCall {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                workspace: workspace.clone(),
                args,
                working_dir,
            })
            .await;

        info!(call_id = %call_id, tool = %tool_name, "call enqueued");

        if needs_deadline {
            self.arm_ready_deadline(workspace, tool_name.clone(), call_id.clone());
        }

        // The handle returns before the job is ready: blocking here would make
        // startup uncancelable.
        Ok(Response::new(ToolCallHandle { call_id }))
    }

    async fn await_tool_result(
        &self,
        request: Request<AwaitToolResultRequest>,
    ) -> Result<Response<Self::AwaitToolResultStream>, Status> {
        let workspace = self.verify_workspace_required(&request).await?;
        let call_id = request.into_inner().call_id;

        if !self
            .caller_owns_call(&workspace, &call_id, "await_tool_result")
            .await
        {
            return Err(Status::not_found(format!(
                "no in-flight call for call_id: {call_id}"
            )));
        }

        let result_rx = self.state.take_result_rx(&call_id).await.ok_or_else(|| {
            Status::not_found(format!("no in-flight call for call_id: {call_id}"))
        })?;

        info!(call_id = %call_id, "streaming call result");

        let stream = ReceiverStream::new(result_rx).map(Ok);
        Ok(Response::new(Box::pin(stream)))
    }

    async fn cancel_tool_call(
        &self,
        request: Request<CancelToolCallRequest>,
    ) -> Result<Response<CancelToolCallResponse>, Status> {
        let workspace = self.verify_workspace_required(&request).await?;
        let call_id = request.into_inner().call_id;
        if call_id.is_empty() {
            return Err(Status::invalid_argument("call_id must not be empty"));
        }

        if !self
            .caller_owns_call(&workspace, &call_id, "cancel_tool_call")
            .await
        {
            return Ok(Response::new(CancelToolCallResponse { cancelled: false }));
        }

        let cancelled = self.state.fire_call_cancel(&call_id).await;
        info!(call_id = %call_id, cancelled, "cancel requested");

        Ok(Response::new(CancelToolCallResponse { cancelled }))
    }

    async fn await_tool_cancel(
        &self,
        request: Request<AwaitToolCancelRequest>,
    ) -> Result<Response<ToolCancelSignal>, Status> {
        let workspace = self.verify_workspace_required(&request).await?;
        let call_id = request.into_inner().call_id;

        if !self
            .caller_owns_call(&workspace, &call_id, "await_tool_cancel")
            .await
        {
            return Ok(Response::new(ToolCancelSignal {}));
        }

        if let Some(token) = self.state.call_cancel_token(&call_id).await {
            token.cancelled().await;
        }

        Ok(Response::new(ToolCancelSignal {}))
    }

    async fn get_tool_call(
        &self,
        request: Request<GetToolCallRequest>,
    ) -> Result<Response<ToolCallAssignment>, Status> {
        // The tool-job pod runs as sa-<workspace>, so its verified token
        // binds it to one workspace; it may only dequeue calls its own
        // workspace enqueued, never another workspace's call for the same tool.
        let workspace = self.verify_workspace_required(&request).await?;
        let req = request.into_inner();
        let tool_name = &req.tool_name;

        // Decide by the job id the request carries against the active-job
        // record. A cleared record — the readyTimeout case — refuses, as does a
        // job the record does not name.
        match self.state.get_active_job(&workspace, tool_name).await {
            None => {
                return Err(Status::failed_precondition(format!(
                    "no active job for tool {tool_name}"
                )))
            }
            Some(active) if active.job_id != req.job_id => {
                return Err(Status::failed_precondition(format!(
                    "job_id {} does not match the active job for tool {tool_name}",
                    req.job_id
                )))
            }
            Some(_) => {}
        }

        loop {
            if let Some(call) = self.state.dequeue_call(&workspace, tool_name).await {
                info!(
                    call_id = %call.call_id,
                    job_id = %req.job_id,
                    workspace = %workspace,
                    tool = %tool_name,
                    "dispatching call to runtime"
                );
                return Ok(Response::new(ToolCallAssignment {
                    call_id: call.call_id,
                    working_dir: call.working_dir,
                    args: call.args,
                }));
            }

            // `wait_for_call` shares one Notify across all tool jobs; a
            // (workspace, tool) tool job woken by another key's enqueue finds
            // nothing for its own key and re-waits. The spurious wakeup is
            // benign — the loop simply re-checks and blocks again.
            self.state.wait_for_call().await;
        }
    }

    async fn stream_tool_result(
        &self,
        request: Request<Streaming<ToolResultFrame>>,
    ) -> Result<Response<SendToolResultAck>, Status> {
        // Decompose first (Streaming is not Sync), verify the tool-job token,
        // then forward. The call_id rides the request-metadata header.
        let (metadata, extensions, stream) = {
            let metadata = request.metadata().clone();
            let extensions = request.extensions().clone();
            let stream = request.into_inner();
            (metadata, extensions, stream)
        };
        let mut auth_request = Request::new(());
        *auth_request.metadata_mut() = metadata.clone();
        *auth_request.extensions_mut() = extensions;
        let workspace = self.verify_workspace_required(&auth_request).await?;

        let call_id = metadata
            .get("x-toolset-call-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| Status::invalid_argument("missing x-toolset-call-id metadata header"))?;

        if !self
            .caller_owns_call(&workspace, &call_id, "stream_tool_result")
            .await
        {
            return Err(Status::not_found(format!(
                "no pending result for call_id: {call_id}"
            )));
        }

        self.forward_result_frames(call_id, stream).await
    }

    // =====================================================================
    // Tool discovery: tool-job-facing
    // =====================================================================

    async fn report_discovered_tools(
        &self,
        request: Request<ReportDiscoveredToolsRequest>,
    ) -> Result<Response<ReportDiscoveredToolsAck>, Status> {
        // Tool-job-audience authenticated: the discovery Job presents the
        // tool.toolset token (routed to the tool-job verifier by the audience
        // layer). The report is keyed by toolset name, not workspace.
        let _ = self.verify_workspace_required(&request).await?;
        let req = request.into_inner();

        // Map the reported tools into the registry's shape, rejecting a
        // malformed arg type as a terminal request error BEFORE any registration
        // so no partial tool set lands.
        let mut tools = Vec::with_capacity(req.tools.len());
        for tool in req.tools {
            let mut args = Vec::with_capacity(tool.args.len());
            for a in tool.args {
                let ty = parse_arg_type(&a.r#type).ok_or_else(|| {
                    Status::invalid_argument(format!(
                        "tool '{}' arg '{}' has unknown type '{}' (expected string, integer, number, boolean)",
                        tool.name, a.name, a.r#type
                    ))
                })?;
                args.push(ArgDecl {
                    name: a.name,
                    ty,
                    required: a.required,
                    env: a.env,
                    description: (!a.description.is_empty()).then_some(a.description),
                });
            }
            let description = if tool.description.is_empty() {
                format!("Invokes the {} tool.", tool.name)
            } else {
                tool.description
            };
            tools.push(RegisteredTool {
                name: tool.name,
                toolset_name: req.toolset_name.clone(),
                description,
                args,
            });
        }

        let count = tools.len();
        self.state
            .set_tools_for_toolset(&req.toolset_name, tools)
            .await;
        info!(toolset = %req.toolset_name, count, "registered reported tools");
        Ok(Response::new(ReportDiscoveredToolsAck {}))
    }
}

/// Parse a reported arg-type string back into an [`ArgType`], the inverse of
/// [`ArgType::as_schema_str`]. An unrecognized string is a terminal request
/// error (the caller rejects with `InvalidArgument`).
fn parse_arg_type(s: &str) -> Option<ArgType> {
    match s {
        "string" => Some(ArgType::String),
        "integer" => Some(ArgType::Integer),
        "number" => Some(ArgType::Number),
        "boolean" => Some(ArgType::Boolean),
        _ => None,
    }
}

impl ControllerService {
    /// Bound this call's wait for its job to become ready. On expiry the call is
    /// failed with the terminal the harness stream already understands, its
    /// bookkeeping is dropped, and its job is deleted. A `false` from
    /// `remove_pending_call` means the job dequeued the call first and won the
    /// race, so the deadline does nothing.
    fn arm_ready_deadline(&self, workspace: String, tool_name: String, call_id: String) {
        // The bound runs from the moment the call is enqueued, not from whenever
        // the executor first polls the task.
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        let state = self.state.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            if !state
                .remove_pending_call(&workspace, &tool_name, &call_id)
                .await
            {
                return;
            }

            let job_name = state
                .get_active_job(&workspace, &tool_name)
                .await
                .map(|j| j.job_name);
            tracing::warn!(
                call_id = %call_id,
                tool = %tool_name,
                workspace = %workspace,
                job = job_name.as_deref().unwrap_or("<none>"),
                "tool Job did not ask for work within readyTimeout; failing the call"
            );

            // Drops the parked receiver, the cancel token, and the ownership
            // record, so a call that never reaches finish_call leaves none.
            state.finish_call(&call_id).await;
            // The guard's Drop emits the synthetic FAILED terminal.
            drop(state.take_result_tx(&call_id).await);
            state.remove_active_job(&workspace, &tool_name).await;

            if let (Some(client), Some(job_name)) = (state.kube_client(), job_name) {
                delete_job_logged(client, state.namespace(), &job_name).await;
            }
        });
    }

    /// Forward a runtime's inbound frame stream to the call's parked
    /// `AwaitToolResult` server-stream, then retire the call. Extracted so the
    /// forward/terminal/cleanup logic is unit-testable with a synthetic stream.
    async fn forward_result_frames<S>(
        &self,
        call_id: String,
        mut stream: S,
    ) -> Result<Response<SendToolResultAck>, Status>
    where
        S: Stream<Item = Result<ToolResultFrame, Status>> + Unpin,
    {
        let (mut guard, (workspace, tool_name)) =
            self.state.take_result_tx(&call_id).await.ok_or_else(|| {
                Status::not_found(format!("no pending result for call_id: {call_id}"))
            })?;

        info!(call_id = %call_id, "receiving tool result stream");

        let mut saw_terminal = false;
        while let Some(frame) = stream.next().await {
            let frame = frame.map_err(|e| Status::internal(format!("frame stream error: {e}")))?;
            if matches!(frame.frame, Some(Frame::Complete(_))) {
                saw_terminal = true;
            }
            let _ = guard.sender().send(frame).await;
        }

        if saw_terminal {
            guard.mark_complete();
        }
        drop(guard);

        self.state.finish_call(&call_id).await;

        if !tool_name.is_empty() {
            self.state.bump_last_activity(&workspace, &tool_name).await;
        }

        Ok(Response::new(SendToolResultAck {}))
    }
}

/// Per-chunk forward budget for the hand-off to the harness's Turn stream. Kept
/// ABOVE the harness's idle-gap so the controller defers to the consumer's own
/// timeout: a consumer that pauses then recovers still gets its reply, and one
/// that genuinely gives up drops its stream (making the next forward fail
/// `Closed` immediately).
const FORWARD_GAP: std::time::Duration = std::time::Duration::from_secs(60);

/// Bound on every wait for a job to become ready, where ready means the job has
/// connected and asked for work — not pod scheduled, not Job created. Running
/// work carries no time bound. A baked default now; a per-toolset-image config
/// key later.
pub const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Forward one chunk to the Turn caller, bounded by `FORWARD_GAP`. Returns
/// `false` when the consumer is gone so the caller stops forwarding and drains
/// the prompt job's stream to EOF — which returns the keepalive prompt job to `GetTurn`.
async fn forward_chunk(active: &ActiveTurn, chunk: TurnResultChunk) -> bool {
    active
        .result_tx
        .sender()
        .send_timeout(chunk, FORWARD_GAP)
        .await
        .is_ok()
}

/// Drive the prompt job's `stream_turn_result` chunk stream: forward chunks to the
/// harness as they arrive and surface a prompt-job-reported `TurnError` as FAILED.
/// The controller persists nothing. Runs cleanup on EVERY exit so no path can
/// leak the per-turn cancel token.
async fn drive_turn_result_stream<S>(
    state: &ControllerState,
    stream: S,
    mut active: ActiveTurn,
    model: &str,
) -> Result<Response<TurnAck>, Status>
where
    S: futures::Stream<Item = Result<TurnResultChunk, Status>>,
{
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
    let mut prompt_job_error: Option<toolset_proto::TurnError> = None;
    let mut downstream_alive = true;
    let mut terminal_delivered = false;

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| Status::internal(format!("stream error: {e}")))?;
        // Buffer the terminal Complete so the reply lands before it; everything
        // else is forwarded immediately for streaming UX.
        if let Some(turn_result_chunk::Chunk::Complete(_)) = &chunk.chunk {
            complete_chunk = Some(chunk);
            continue;
        }
        let is_error = matches!(&chunk.chunk, Some(turn_result_chunk::Chunk::Error(_)));
        if let Some(turn_result_chunk::Chunk::Error(e)) = &chunk.chunk {
            prompt_job_error = Some(e.clone());
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
                    "turn consumer stopped draining; draining the prompt job to EOF to free the keepalive job",
                );
            }
        }
    }

    let stranded = if let Some(err) = prompt_job_error {
        tracing::warn!(
            workspace = %active.workspace,
            conversation_id = %active.conversation_id,
            code = err.code,
            error = %err.message,
            "turn failed: prompt job reported an error",
        );
        !terminal_delivered
    } else if let Some(complete_chunk) = complete_chunk {
        let delivered = downstream_alive && forward_chunk(active, complete_chunk).await;
        state.bump_model_activity(model).await;
        !delivered
    } else {
        false
    };
    if !stranded {
        active.result_tx.mark_complete();
    }
    Ok(Response::new(TurnAck {}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ToolsetEntry;
    use crate::registry::{ArgDecl, ArgType};
    use crate::state::{PromptConfig, PromptJobState, RegisteredTool, TurnResultGuard};
    use proto_common::{ToolComplete, ToolOutcome};
    use shared::auth::TokenVerifier;

    // ---- Shared test helpers ----

    fn test_state() -> Arc<ControllerState> {
        ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        )
    }

    struct FixedWorkspaceVerifier(String);

    #[tonic::async_trait]
    impl TokenVerifier for FixedWorkspaceVerifier {
        async fn verify_token(&self, _token: &str) -> Result<String, Status> {
            Ok(self.0.clone())
        }
    }

    fn fixed_pair(name: &str) -> VerifierPair {
        VerifierPair {
            harness: Arc::new(FixedWorkspaceVerifier(name.to_string())),
            tool_job: Arc::new(FixedWorkspaceVerifier(name.to_string())),
        }
    }

    /// Request stamped with the harness audience extension (matching the
    /// audience layer) plus a bearer token.
    fn authed<T>(inner: T) -> Request<T> {
        let mut req = Request::new(inner);
        req.metadata_mut()
            .insert("authorization", "Bearer test".parse().unwrap());
        req.extensions_mut().insert(RequiredAudience::Harness);
        req
    }

    fn make_service(state: Arc<ControllerState>) -> ControllerService {
        ControllerService::new(
            state,
            None,
            WorkspaceBindings::empty(),
            PromptConfig::empty(),
        )
    }

    // ---- Tool dispatch tests ----

    fn arg(name: &str, ty: ArgType, required: bool, env: &str) -> ArgDecl {
        ArgDecl {
            name: name.to_string(),
            ty,
            required,
            env: env.to_string(),
            description: None,
        }
    }

    async fn register_tool_with_args(
        state: &ControllerState,
        toolset: &str,
        name: &str,
        desc: &str,
        args: Vec<ArgDecl>,
    ) {
        state
            .set_tools_for_toolset(
                toolset,
                vec![RegisteredTool {
                    name: name.to_string(),
                    toolset_name: toolset.to_string(),
                    description: desc.to_string(),
                    args,
                }],
            )
            .await;
    }

    async fn register_tools(state: &ControllerState, toolset: &str, tools: Vec<(&str, &str)>) {
        let registered: Vec<RegisteredTool> = tools
            .into_iter()
            .map(|(name, desc)| RegisteredTool {
                name: name.to_string(),
                toolset_name: toolset.to_string(),
                description: desc.to_string(),
                args: vec![],
            })
            .collect();
        state.set_tools_for_toolset(toolset, registered).await;
    }

    fn make_toolset(_name: &str) -> ToolsetEntry {
        ToolsetEntry::default()
    }

    fn stdout_frame(text: &str) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Stdout(text.into())),
        }
    }

    fn complete_frame(is_error: bool, exit_code: i32) -> ToolResultFrame {
        let outcome = if is_error {
            ToolOutcome::Failed
        } else {
            ToolOutcome::Done
        };
        ToolResultFrame {
            frame: Some(Frame::Complete(ToolComplete {
                outcome: outcome as i32,
                exit_code,
            })),
        }
    }

    fn frame_stream(
        frames: Vec<ToolResultFrame>,
    ) -> impl Stream<Item = Result<ToolResultFrame, Status>> + Unpin {
        futures::stream::iter(frames.into_iter().map(Ok))
    }

    async fn drain_frames<S>(mut stream: S) -> Vec<ToolResultFrame>
    where
        S: Stream<Item = Result<ToolResultFrame, Status>> + Unpin,
    {
        let mut out = Vec::new();
        while let Some(f) = stream.next().await {
            out.push(f.expect("frame stream must not error"));
        }
        out
    }

    #[tokio::test]
    async fn call_tool_unknown_returns_not_found() {
        let svc = ControllerService::new(
            test_state(),
            Some(fixed_pair("test")),
            WorkspaceBindings::empty(),
            PromptConfig::empty(),
        );
        let err = svc
            .begin_tool_call(authed(CallToolRequest {
                name: "nonexistent".to_string(),
                input_json: "{}".to_string(),
                conversation_id: String::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn call_tool_missing_toolset_returns_failed_precondition() {
        let state = test_state();
        register_tools(&state, "test-toolset", vec![("echo", "Echo tool")]).await;

        let mut bindings_map = std::collections::HashMap::new();
        bindings_map.insert("test".to_string(), vec!["test-toolset".to_string()]);
        let svc = ControllerService::new(
            state,
            Some(fixed_pair("test")),
            WorkspaceBindings::from_map(bindings_map),
            PromptConfig::empty(),
        );
        let err = svc
            .begin_tool_call(authed(CallToolRequest {
                name: "echo".to_string(),
                input_json: "{}".to_string(),
                conversation_id: String::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    /// The workspace every `ready_service` caller authenticates as. Bound to
    /// `test-toolset` so the fail-closed binding check in `begin_tool_call`
    /// admits it.
    const READY_WORKSPACE: &str = "test";

    /// Populates the given state with the `echo` tool (its `MESSAGE` arg and
    /// the `test-toolset` entry) and returns bindings mapping
    /// [`READY_WORKSPACE`] plus any extra workspaces to that toolset.
    async fn echo_state(state: &ControllerState, extra_workspaces: &[&str]) -> WorkspaceBindings {
        register_tool_with_args(
            state,
            "test-toolset",
            "echo",
            "Echo tool",
            vec![arg("message", ArgType::String, true, "MESSAGE")],
        )
        .await;
        state
            .set_toolset("test-toolset".into(), make_toolset("test-toolset"))
            .await;
        let mut bindings_map = std::collections::HashMap::new();
        bindings_map.insert(
            READY_WORKSPACE.to_string(),
            vec!["test-toolset".to_string()],
        );
        for workspace in extra_workspaces {
            bindings_map.insert(workspace.to_string(), vec!["test-toolset".to_string()]);
        }
        WorkspaceBindings::from_map(bindings_map)
    }

    async fn ready_service() -> Arc<ControllerService> {
        let state = test_state();
        let bindings = echo_state(&state, &[]).await;
        // The job `job-1` is already up and serving `echo`: `get_tool_call`
        // decides by the job id the request carries against this record.
        state
            .set_active_job(ActiveJob {
                job_name: "job-1".into(),
                job_id: "job-1".into(),
                tool_name: "echo".into(),
                workspace: READY_WORKSPACE.into(),
                last_activity: std::time::Instant::now(),
                keepalive_seconds: TOOL_KEEPALIVE_IDLE_SECONDS,
            })
            .await;
        Arc::new(ControllerService::new(
            state,
            Some(fixed_pair(READY_WORKSPACE)),
            bindings,
            PromptConfig::empty(),
        ))
    }

    fn echo_request() -> CallToolRequest {
        CallToolRequest {
            name: "echo".to_string(),
            input_json: r#"{"message":"hello"}"#.to_string(),
            conversation_id: String::new(),
        }
    }

    #[tokio::test]
    async fn call_tool_round_trip() {
        let svc = ready_service().await;

        let handle = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .expect("begin_tool_call must not block on the result")
            .into_inner();

        let assignment = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            svc.get_tool_call(authed(GetToolCallRequest {
                job_id: "job-1".to_string(),
                tool_name: "echo".to_string(),
            })),
        )
        .await
        .expect("get_tool_call timed out")
        .unwrap()
        .into_inner();

        assert_eq!(assignment.args.get("MESSAGE"), Some(&"hello".to_string()));
        assert_eq!(assignment.call_id, handle.call_id);

        let result_stream = svc
            .await_tool_result(authed(AwaitToolResultRequest {
                call_id: handle.call_id.clone(),
                conversation_id: String::new(),
            }))
            .await
            .expect("await_tool_result must return the stream")
            .into_inner();

        svc.forward_result_frames(
            assignment.call_id,
            frame_stream(vec![stdout_frame("hello"), complete_frame(false, 0)]),
        )
        .await
        .expect("forwarding the runtime's frames must succeed");

        let frames = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            drain_frames(result_stream),
        )
        .await
        .expect("draining the result stream timed out");

        assert!(
            matches!(frames.first().and_then(|f| f.frame.as_ref()), Some(Frame::Stdout(s)) if s == "hello")
        );
        match frames.last().and_then(|f| f.frame.as_ref()) {
            Some(Frame::Complete(c)) => {
                assert_eq!(c.outcome(), ToolOutcome::Done);
                assert_eq!(c.exit_code, 0);
            }
            other => panic!("the last frame must be the terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn forward_result_frames_bumps_keepalive_on_completion() {
        let svc = ready_service().await;

        let handle = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .expect("begin_tool_call must enqueue")
            .into_inner();

        let stale = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .expect("clock has enough uptime to subtract an hour");
        svc.state
            .set_active_job(ActiveJob {
                job_name: "job-1".into(),
                job_id: "job-1".into(),
                tool_name: "echo".into(),
                workspace: READY_WORKSPACE.into(),
                last_activity: stale,
                keepalive_seconds: 300,
            })
            .await;

        let _result_stream = svc
            .await_tool_result(authed(AwaitToolResultRequest {
                call_id: handle.call_id.clone(),
                conversation_id: String::new(),
            }))
            .await
            .expect("await_tool_result must return the stream")
            .into_inner();

        svc.forward_result_frames(
            handle.call_id.clone(),
            frame_stream(vec![stdout_frame("hello"), complete_frame(false, 0)]),
        )
        .await
        .expect("forwarding the runtime's frames must succeed");

        let job = svc
            .state
            .get_active_job(READY_WORKSPACE, "echo")
            .await
            .expect("the ActiveJob must still exist after forwarding");
        assert!(
            job.last_activity > stale,
            "completing the result stream must bump the tool's last_activity"
        );
    }

    #[tokio::test]
    async fn get_tool_call_blocks_until_enqueued() {
        let svc = ready_service().await;

        let svc_for_get = svc.clone();
        let get_handle = tokio::spawn(async move {
            svc_for_get
                .get_tool_call(authed(GetToolCallRequest {
                    job_id: "job-1".to_string(),
                    tool_name: "echo".to_string(),
                }))
                .await
        });

        tokio::task::yield_now().await;
        assert!(!get_handle.is_finished(), "GetToolCall should be blocking");

        let svc_for_call = svc.clone();
        tokio::spawn(async move {
            let _ = svc_for_call.begin_tool_call(authed(echo_request())).await;
        });

        let assignment = tokio::time::timeout(std::time::Duration::from_secs(2), get_handle)
            .await
            .expect("GetToolCall should resolve within timeout")
            .unwrap()
            .unwrap()
            .into_inner();

        assert_eq!(assignment.args.get("MESSAGE"), Some(&"hello".to_string()));
    }

    #[tokio::test]
    async fn stream_result_unknown_call_id() {
        let svc = make_service(test_state());
        let err = svc
            .forward_result_frames(
                "nonexistent".to_string(),
                frame_stream(vec![complete_frame(false, 0)]),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn watch_tools_emits_initial_snapshot() {
        use futures::StreamExt;
        let state = test_state();
        register_tools(&state, "c1", vec![("git", "push commits")]).await;

        let svc = make_service(state);
        let resp = svc
            .watch_tools(Request::new(WatchToolsRequest {}))
            .await
            .unwrap();
        let mut stream = resp.into_inner();

        let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("watch_tools must yield initial snapshot")
            .expect("stream not closed")
            .expect("ok response");
        assert_eq!(first.tools.len(), 1);
        assert_eq!(first.tools[0].name, "git");
    }

    #[tokio::test]
    async fn watch_tools_emits_update_on_toolset_change() {
        use futures::StreamExt;
        let state = test_state();
        let svc = make_service(state.clone());
        let resp = svc
            .watch_tools(Request::new(WatchToolsRequest {}))
            .await
            .unwrap();
        let mut stream = resp.into_inner();

        let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(first.tools.is_empty());

        register_tools(&state, "c1", vec![("git", "push commits")]).await;

        let second = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("watch_tools must push update after set_tools_for_toolset")
            .expect("stream not closed")
            .expect("ok response");
        assert_eq!(second.tools.len(), 1);
        assert_eq!(second.tools[0].name, "git");
    }

    #[tokio::test]
    async fn call_tool_unauthorized_toolset_returns_permission_denied() {
        let state = test_state();
        register_tools(&state, "git", vec![("git-push", "Push commits")]).await;
        state.set_toolset("git".into(), make_toolset("git")).await;

        let mut bindings_map = std::collections::HashMap::new();
        bindings_map.insert("alpha".to_string(), vec!["ssh".to_string()]);
        let bindings = WorkspaceBindings::from_map(bindings_map);

        let svc = ControllerService::new(
            state,
            Some(fixed_pair("alpha")),
            bindings,
            PromptConfig::empty(),
        );

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            svc.begin_tool_call(authed(CallToolRequest {
                name: "git-push".to_string(),
                input_json: "{}".to_string(),
                conversation_id: String::new(),
            })),
        )
        .await
        .expect("begin_tool_call should reject immediately, not block");
        assert_eq!(result.unwrap_err().code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn cancel_of_unknown_call_id_is_a_safe_no_op() {
        let svc = ready_service().await;

        let handle = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner();

        let unknown = svc
            .cancel_tool_call(authed(CancelToolCallRequest {
                call_id: "does-not-exist".to_string(),
            }))
            .await
            .expect("cancel of an unknown id must be Ok, never an error status")
            .into_inner();
        assert!(
            !unknown.cancelled,
            "an unknown/finished call reports cancelled=false"
        );

        let real = svc
            .cancel_tool_call(authed(CancelToolCallRequest {
                call_id: handle.call_id,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(
            real.cancelled,
            "the genuine in-flight call must still be cancellable after the no-op"
        );
    }

    #[tokio::test]
    async fn await_tool_result_unblocks_on_dropped_result_stream() {
        let svc = ready_service().await;

        let handle = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner();
        let call_id = handle.call_id.clone();

        let result_stream = svc
            .await_tool_result(authed(AwaitToolResultRequest {
                call_id: call_id.clone(),
                conversation_id: String::new(),
            }))
            .await
            .expect("await_tool_result must return the stream")
            .into_inner();

        svc.forward_result_frames(call_id, frame_stream(Vec::new()))
            .await
            .unwrap();

        let frames = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            drain_frames(result_stream),
        )
        .await
        .expect("a dropped result stream must unblock the parked awaiter with a terminal");
        match frames.last().and_then(|f| f.frame.as_ref()) {
            Some(Frame::Complete(c)) => {
                assert_eq!(
                    c.outcome(),
                    ToolOutcome::Failed,
                    "a runtime that vanished mid-stream surfaces as a terminal failure"
                );
                assert_eq!(
                    c.exit_code, -1,
                    "the synthetic terminal carries the -1 sentinel"
                );
            }
            other => panic!("the parked stream must terminate in a ToolComplete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn await_tool_cancel_returns_when_cancel_fires() {
        let svc = ready_service().await;

        let handle = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner();
        let call_id = handle.call_id.clone();

        let svc_poll = svc.clone();
        let poll_call_id = call_id.clone();
        let cancel_poll = tokio::spawn(async move {
            svc_poll
                .await_tool_cancel(authed(AwaitToolCancelRequest {
                    call_id: poll_call_id,
                }))
                .await
        });

        tokio::task::yield_now().await;
        assert!(
            !cancel_poll.is_finished(),
            "AwaitToolCancel must block until a cancel fires"
        );

        svc.cancel_tool_call(authed(CancelToolCallRequest { call_id }))
            .await
            .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), cancel_poll)
            .await
            .expect("AwaitToolCancel must return once the cancel fires")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn await_tool_cancel_rejects_missing_token() {
        // With a verifier configured, the tool-job-audience stamp is enforced via
        // verify_workspace_required. A request carrying no bearer token must be
        // rejected before the handler acts on call_id.
        let svc = ControllerService::new(
            test_state(),
            Some(fixed_pair("ws")),
            WorkspaceBindings::empty(),
            PromptConfig::empty(),
        );

        let status = svc
            .await_tool_cancel(Request::new(AwaitToolCancelRequest {
                call_id: "any-call".into(),
            }))
            .await
            .expect_err("await_tool_cancel must reject a request with no tool-job token");

        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn await_tool_result_rejects_missing_token() {
        // With a verifier configured, the harness-audience stamp is enforced via
        // verify_workspace_required. A request carrying no bearer token must be
        // rejected before the handler consumes the result receiver.
        let svc = ControllerService::new(
            test_state(),
            Some(fixed_pair("ws")),
            WorkspaceBindings::empty(),
            PromptConfig::empty(),
        );

        let status = svc
            .await_tool_result(Request::new(AwaitToolResultRequest {
                call_id: "any-call".into(),
                conversation_id: String::new(),
            }))
            .await
            .err()
            .expect("await_tool_result must reject a request with no harness token");

        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn cancel_tool_call_rejects_missing_token() {
        // With a verifier configured, the harness-audience stamp is enforced via
        // verify_workspace_required. A request carrying no bearer token must be
        // rejected before the handler fires the cancel.
        let svc = ControllerService::new(
            test_state(),
            Some(fixed_pair("ws")),
            WorkspaceBindings::empty(),
            PromptConfig::empty(),
        );

        let status = svc
            .cancel_tool_call(Request::new(CancelToolCallRequest {
                call_id: "any-call".into(),
            }))
            .await
            .expect_err("cancel_tool_call must reject a request with no harness token");

        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }

    // ---- Tool-call ownership ----

    /// A second workspace, bound to the SAME toolset as [`READY_WORKSPACE`], so
    /// every refusal below is an OWNERSHIP refusal and never the binding check
    /// standing in for one.
    const INTRUDER_WORKSPACE: &str = "intruder";

    /// Two services over ONE `ControllerState`, authenticating as two different
    /// workspaces.
    async fn owner_and_intruder() -> (Arc<ControllerService>, Arc<ControllerService>) {
        let state = test_state();
        let bindings = echo_state(&state, &[INTRUDER_WORKSPACE]).await;
        let owner = Arc::new(ControllerService::new(
            state.clone(),
            Some(fixed_pair(READY_WORKSPACE)),
            bindings.clone(),
            PromptConfig::empty(),
        ));
        let intruder = Arc::new(ControllerService::new(
            state,
            Some(fixed_pair(INTRUDER_WORKSPACE)),
            bindings,
            PromptConfig::empty(),
        ));
        (owner, intruder)
    }

    /// A `Streaming<ToolResultFrame>` over an in-memory gRPC-framed body, so the
    /// real `stream_tool_result` handler — auth, header read, ownership — is
    /// exercised instead of `forward_result_frames` alone.
    fn job_result_stream(
        call_id: &str,
        frames: Vec<ToolResultFrame>,
    ) -> Request<Streaming<ToolResultFrame>> {
        use prost::Message;
        use tonic::codec::{Codec, ProstCodec};

        let mut wire = Vec::new();
        for frame in frames {
            let mut body = Vec::new();
            frame.encode(&mut body).expect("frame encodes");
            wire.push(0u8); // no compression
            wire.extend_from_slice(&(body.len() as u32).to_be_bytes());
            wire.extend_from_slice(&body);
        }
        let mut codec: ProstCodec<ToolResultFrame, ToolResultFrame> = ProstCodec::default();
        let stream = Streaming::new_request(
            codec.decoder(),
            http_body_util::Full::new(prost::bytes::Bytes::from(wire)),
            None,
            None,
        );
        let mut req = Request::new(stream);
        req.metadata_mut()
            .insert("authorization", "Bearer test".parse().unwrap());
        req.metadata_mut()
            .insert("x-toolset-call-id", call_id.parse().unwrap());
        req.extensions_mut().insert(RequiredAudience::ToolJob);
        req
    }

    fn await_result_request(call_id: &str) -> AwaitToolResultRequest {
        AwaitToolResultRequest {
            call_id: call_id.to_string(),
            conversation_id: String::new(),
        }
    }

    #[tokio::test]
    async fn await_tool_result_cross_workspace_returns_not_found() {
        let (owner, intruder) = owner_and_intruder().await;
        let call_id = owner
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        let status = intruder
            .await_tool_result(authed(await_result_request(&call_id)))
            .await
            .err()
            .expect("a non-owner must not receive another workspace's result stream");
        assert_eq!(
            status.code(),
            tonic::Code::NotFound,
            "a non-owner must get exactly the unknown-call-id outcome, learning nothing"
        );

        assert!(
            owner
                .await_tool_result(authed(await_result_request(&call_id)))
                .await
                .is_ok(),
            "the refusal must not consume the owner's parked receiver"
        );
    }

    #[tokio::test]
    async fn await_tool_result_owner_receives_its_own_result() {
        let (owner, _intruder) = owner_and_intruder().await;
        let call_id = owner
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        let stream = owner
            .await_tool_result(authed(await_result_request(&call_id)))
            .await
            .expect("the owning workspace must receive its own call's result")
            .into_inner();

        owner
            .forward_result_frames(
                call_id,
                frame_stream(vec![stdout_frame("hello"), complete_frame(false, 0)]),
            )
            .await
            .unwrap();

        let frames = tokio::time::timeout(std::time::Duration::from_secs(2), drain_frames(stream))
            .await
            .expect("draining the owner's result stream timed out");
        assert!(
            matches!(frames.first().and_then(|f| f.frame.as_ref()), Some(Frame::Stdout(s)) if s == "hello")
        );
    }

    #[tokio::test]
    async fn cancel_tool_call_cross_workspace_is_a_noop() {
        let (owner, intruder) = owner_and_intruder().await;
        let call_id = owner
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        let resp = intruder
            .cancel_tool_call(authed(CancelToolCallRequest {
                call_id: call_id.clone(),
            }))
            .await
            .expect("a non-owner's cancel must be Ok, exactly as an unknown id is")
            .into_inner();
        assert!(
            !resp.cancelled,
            "a non-owner must get the same success-with-nothing-cancelled outcome an unknown call id produces"
        );
    }

    #[tokio::test]
    async fn cancel_tool_call_cross_workspace_leaves_owner_cancel_unfired() {
        let (owner, intruder) = owner_and_intruder().await;
        let call_id = owner
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        let _ = intruder
            .cancel_tool_call(authed(CancelToolCallRequest {
                call_id: call_id.clone(),
            }))
            .await;

        let token = owner
            .state
            .call_cancel_token(&call_id)
            .await
            .expect("the owner's cancel token must survive a non-owner's cancel attempt");
        assert!(
            !token.is_cancelled(),
            "a non-owner must not fire the owner's cancel signal"
        );
    }

    #[tokio::test]
    async fn await_tool_cancel_cross_workspace_returns_immediately() {
        let (owner, intruder) = owner_and_intruder().await;
        let call_id = owner
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            intruder.await_tool_cancel(authed(AwaitToolCancelRequest { call_id })),
        )
        .await
        .expect("a non-owner must get the empty cancel signal immediately, never park on the owner's token")
        .expect("the refusal is the unknown-call-id outcome: Ok, not an error status");
    }

    #[tokio::test]
    async fn stream_tool_result_cross_workspace_returns_not_found() {
        let (owner, intruder) = owner_and_intruder().await;
        let call_id = owner
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        let status = intruder
            .stream_tool_result(job_result_stream(
                &call_id,
                vec![stdout_frame("injected"), complete_frame(false, 0)],
            ))
            .await
            .err()
            .expect("a non-owner's job must not stream into another workspace's call");
        assert_eq!(status.code(), tonic::Code::NotFound);

        assert!(
            owner.state.take_result_tx(&call_id).await.is_some(),
            "the refusal must leave the owner's result sender intact"
        );
    }

    #[tokio::test]
    async fn stream_tool_result_owner_job_round_trips() {
        let (owner, _intruder) = owner_and_intruder().await;
        let call_id = owner
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        let stream = owner
            .await_tool_result(authed(await_result_request(&call_id)))
            .await
            .unwrap()
            .into_inner();

        owner
            .stream_tool_result(job_result_stream(
                &call_id,
                vec![stdout_frame("from-job"), complete_frame(false, 0)],
            ))
            .await
            .expect("the owning workspace's job must be able to stream its result");

        let frames = tokio::time::timeout(std::time::Duration::from_secs(2), drain_frames(stream))
            .await
            .expect("draining the owner's result stream timed out");
        assert!(
            matches!(frames.first().and_then(|f| f.frame.as_ref()), Some(Frame::Stdout(s)) if s == "from-job"),
            "the owner receives its job's result unchanged"
        );
        match frames.last().and_then(|f| f.frame.as_ref()) {
            Some(Frame::Complete(c)) => {
                assert_eq!(c.outcome(), ToolOutcome::Done);
                assert_eq!(c.exit_code, 0);
            }
            other => panic!("the terminal must arrive unchanged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn await_tool_result_requires_a_verified_workspace() {
        let svc = make_service(test_state());
        let status = svc
            .await_tool_result(authed(await_result_request("any-call")))
            .await
            .err()
            .expect("no verifier means no caller workspace, so the call must be refused");
        assert_eq!(
            status.code(),
            tonic::Code::FailedPrecondition,
            "verification is required, not optional"
        );
    }

    #[tokio::test]
    async fn cancel_tool_call_requires_a_verified_workspace() {
        let svc = make_service(test_state());
        let status = svc
            .cancel_tool_call(authed(CancelToolCallRequest {
                call_id: "any-call".into(),
            }))
            .await
            .err()
            .expect("no verifier means no caller workspace, so the cancel must be refused");
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn await_tool_cancel_requires_a_verified_workspace() {
        let svc = make_service(test_state());
        let status = svc
            .await_tool_cancel(authed(AwaitToolCancelRequest {
                call_id: "any-call".into(),
            }))
            .await
            .err()
            .expect("no verifier means no caller workspace, so the long-poll must be refused");
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn stream_tool_result_requires_a_verified_workspace() {
        let svc = make_service(test_state());
        let status = svc
            .stream_tool_result(job_result_stream(
                "any-call",
                vec![complete_frame(false, 0)],
            ))
            .await
            .err()
            .expect("no verifier means no caller workspace, so the result stream must be refused");
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    }

    /// Collects the rendered fields of every WARN event emitted while the
    /// returned guard is alive. Built on `tracing-subscriber`, already a
    /// dependency of this crate — no logging dependency is added for this.
    #[derive(Clone, Default)]
    struct WarnLog(Arc<std::sync::Mutex<Vec<String>>>);

    impl WarnLog {
        fn contains_all(&self, needles: &[&str]) -> bool {
            self.0
                .lock()
                .unwrap()
                .iter()
                .any(|line| needles.iter().all(|n| line.contains(n)))
        }

        fn rendered(&self) -> String {
            self.0.lock().unwrap().join("\n")
        }
    }

    struct WarnCaptureLayer(WarnLog);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarnCaptureLayer {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *event.metadata().level() != tracing::Level::WARN {
                return;
            }
            let mut fields = FieldRenderer(String::new());
            event.record(&mut fields);
            self.0 .0.lock().unwrap().push(fields.0);
        }
    }

    struct FieldRenderer(String);

    impl tracing::field::Visit for FieldRenderer {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;
            let _ = write!(self.0, "{}={:?} ", field.name(), value);
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            use std::fmt::Write;
            let _ = write!(self.0, "{}={} ", field.name(), value);
        }
    }

    #[tokio::test]
    async fn mismatch_logs_a_warning_naming_the_caller_and_owner() {
        use tracing_subscriber::layer::SubscriberExt;

        let warnings = WarnLog::default();
        let subscriber = tracing_subscriber::registry().with(WarnCaptureLayer(warnings.clone()));
        let _guard = tracing::subscriber::set_default(subscriber);

        let (owner, intruder) = owner_and_intruder().await;
        let call_id = owner
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        let _ = intruder
            .await_tool_result(authed(await_result_request(&call_id)))
            .await;

        assert!(
            warnings.contains_all(&[INTRUDER_WORKSPACE, READY_WORKSPACE]),
            "the denial must be recorded as a warning naming both the caller and the \
             attempted owner (a silent refusal is invisible to the operator); captured:\n{}",
            warnings.rendered()
        );
    }

    // ---- readyTimeout ----

    /// Records every request the controller makes to the API server. A POST
    /// echoes its body back as 201 so `Api::create` deserializes; a DELETE
    /// answers with a success `Status`; a GET answers from `job_get`, or 404.
    #[derive(Clone, Default)]
    struct KubeCalls(Arc<std::sync::Mutex<Vec<(String, String)>>>);

    impl KubeCalls {
        fn names_for(&self, method: &str) -> Vec<String> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(m, _)| m == method)
                .map(|(_, p)| p.rsplit('/').next().unwrap_or_default().to_string())
                .collect()
        }

        fn deleted_jobs(&self) -> Vec<String> {
            self.names_for("DELETE")
        }

        fn created_jobs(&self) -> usize {
            self.names_for("POST").len()
        }
    }

    fn mock_kube_client(calls: KubeCalls, job_get: Option<serde_json::Value>) -> kube::Client {
        use http_body_util::BodyExt;
        use kube::client::Body as KubeBody;

        let svc = tower::service_fn(move |req: http::Request<KubeBody>| {
            let calls = calls.clone();
            let job_get = job_get.clone();
            async move {
                let (parts, body) = req.into_parts();
                let bytes = body.collect().await.expect("collect body").to_bytes();
                calls.0.lock().unwrap().push((
                    parts.method.as_str().to_string(),
                    parts.uri.path().to_string(),
                ));
                let (code, payload) = match parts.method {
                    http::Method::POST => (201u16, bytes.to_vec()),
                    http::Method::DELETE => (
                        200,
                        br#"{"kind":"Status","apiVersion":"v1","status":"Success"}"#.to_vec(),
                    ),
                    _ => match &job_get {
                        Some(job) => (200, serde_json::to_vec(job).expect("job serializes")),
                        None => (
                            404,
                            br#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":404,"reason":"NotFound","message":"jobs not found"}"#
                                .to_vec(),
                        ),
                    },
                };
                let resp = http::Response::builder()
                    .status(code)
                    .header("content-type", "application/json")
                    .body(KubeBody::from(payload))
                    .expect("build response");
                Ok::<_, std::convert::Infallible>(resp)
            }
        });
        kube::Client::new(svc, "test-ns")
    }

    /// A Job the health probe classifies as `Running`: active, started long
    /// before `STARTUP_GRACE`.
    fn running_job_json(name: &str) -> serde_json::Value {
        serde_json::json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": { "name": name, "namespace": "test-ns" },
            "status": { "active": 1, "startTime": "2020-01-01T00:00:00Z" }
        })
    }

    fn kube_state(calls: KubeCalls, job_get: Option<serde_json::Value>) -> Arc<ControllerState> {
        ControllerState::new(
            Some(mock_kube_client(calls, job_get)),
            "test-ns".into(),
            "http://toolset-ctrl:9090".into(),
            shared::scheduling::SchedulingConfig::default(),
        )
    }

    /// `ready_service`, but with a kube client, so `begin_tool_call` reaches the
    /// spawn branch that arms the ready deadline.
    async fn spawning_service(
        calls: KubeCalls,
        job_get: Option<serde_json::Value>,
    ) -> Arc<ControllerService> {
        let state = kube_state(calls, job_get);
        let bindings = echo_state(&state, &[]).await;
        Arc::new(ControllerService::new(
            state,
            Some(fixed_pair(READY_WORKSPACE)),
            bindings,
            PromptConfig::empty(),
        ))
    }

    /// Let background tasks armed under a paused clock run to completion.
    async fn settle() {
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn tool_call_without_job_connect_fails_at_ready_timeout() {
        let svc = spawning_service(KubeCalls::default(), None).await;
        let call_id = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;
        let stream = svc
            .await_tool_result(authed(await_result_request(&call_id)))
            .await
            .unwrap()
            .into_inner();

        // No job ever calls GetToolCall.
        tokio::time::advance(READY_TIMEOUT + std::time::Duration::from_secs(1)).await;
        settle().await;

        let frames = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            drain_frames(stream),
        )
        .await
        .expect("a call whose job never asks for work must be failed at readyTimeout, not parked forever");
        match frames.last().and_then(|f| f.frame.as_ref()) {
            Some(Frame::Complete(c)) => {
                assert_eq!(
                    c.outcome(),
                    ToolOutcome::Failed,
                    "expiry surfaces as the terminal failure the harness stream already receives"
                );
                assert_eq!(c.exit_code, -1);
            }
            other => panic!("the expired call must terminate in a ToolComplete, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ready_timeout_deletes_the_job_and_clears_the_active_job_record() {
        let calls = KubeCalls::default();
        let svc = spawning_service(calls.clone(), None).await;
        svc.begin_tool_call(authed(echo_request())).await.unwrap();
        let spawned = svc
            .state
            .get_active_job(READY_WORKSPACE, "echo")
            .await
            .expect("precondition: begin_tool_call records the job it spawned")
            .job_name;

        tokio::time::advance(READY_TIMEOUT + std::time::Duration::from_secs(1)).await;
        settle().await;

        assert!(
            calls.deleted_jobs().contains(&spawned),
            "readyTimeout must delete the call's job, not leave a zombie; deletes seen: {:?}",
            calls.deleted_jobs()
        );
        assert!(
            svc.state
                .get_active_job(READY_WORKSPACE, "echo")
                .await
                .is_none(),
            "readyTimeout must clear the call's active-job record"
        );
    }

    /// The bound is one READY_TIMEOUT, not zero: a call whose job has not yet
    /// asked for work survives right up to its deadline. Without this, a
    /// deadline armed in the past would satisfy every expiry assertion above.
    #[tokio::test(start_paused = true)]
    async fn tool_call_survives_until_its_ready_deadline() {
        let calls = KubeCalls::default();
        let svc = spawning_service(calls.clone(), None).await;
        let call_id = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        tokio::time::advance(READY_TIMEOUT - std::time::Duration::from_secs(1)).await;
        settle().await;

        assert!(
            calls.deleted_jobs().is_empty(),
            "nothing is torn down before the bound elapses; deletes seen: {:?}",
            calls.deleted_jobs()
        );
        assert_eq!(
            svc.state.call_owner(&call_id).await.as_deref(),
            Some(READY_WORKSPACE),
            "the call is still in flight one second before its deadline"
        );
        assert!(
            svc.state
                .get_active_job(READY_WORKSPACE, "echo")
                .await
                .is_some(),
            "the job it is waiting on is still recorded"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ready_timeout_removes_the_calls_owner_entry() {
        let svc = spawning_service(KubeCalls::default(), None).await;
        let call_id = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;
        assert_eq!(
            svc.state.call_owner(&call_id).await.as_deref(),
            Some(READY_WORKSPACE),
            "precondition: the call records its owner"
        );

        tokio::time::advance(READY_TIMEOUT + std::time::Duration::from_secs(1)).await;
        settle().await;

        assert!(
            svc.state.call_owner(&call_id).await.is_none(),
            "a timed-out call never reaches finish_call, so its teardown must remove the \
             ownership record itself — otherwise the entry is orphaned forever"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn late_get_tool_call_with_stale_job_id_is_failed_precondition() {
        let svc = spawning_service(KubeCalls::default(), None).await;
        let call_id = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        tokio::time::advance(READY_TIMEOUT + std::time::Duration::from_secs(1)).await;
        settle().await;

        let err = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            svc.get_tool_call(authed(GetToolCallRequest {
                job_id: call_id,
                tool_name: "echo".to_string(),
            })),
        )
        .await
        .expect("a job arriving after its call expired must be refused, never parked")
        .err()
        .expect("the cleared active-job record must refuse the stale job id");
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test(start_paused = true)]
    async fn tool_call_attaching_to_a_ready_job_starts_no_ready_deadline() {
        let calls = KubeCalls::default();
        let svc = spawning_service(calls.clone(), Some(running_job_json("tool-echo-warm"))).await;
        svc.state
            .set_active_job(ActiveJob {
                job_name: "tool-echo-warm".into(),
                job_id: "warm-call".into(),
                tool_name: "echo".into(),
                workspace: READY_WORKSPACE.into(),
                last_activity: std::time::Instant::now(),
                keepalive_seconds: TOOL_KEEPALIVE_IDLE_SECONDS,
            })
            .await;

        let call_id = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;
        assert_eq!(
            calls.created_jobs(),
            0,
            "precondition: the call attaches to the already-running job"
        );
        let stream = svc
            .await_tool_result(authed(await_result_request(&call_id)))
            .await
            .unwrap()
            .into_inner();

        tokio::time::advance(READY_TIMEOUT * 3).await;
        settle().await;

        assert!(
            calls.deleted_jobs().is_empty(),
            "no ready deadline is armed for a job that is already ready, and running work \
             carries no time bound; deletes seen: {:?}",
            calls.deleted_jobs()
        );
        assert!(
            svc.state
                .get_active_job(READY_WORKSPACE, "echo")
                .await
                .is_some(),
            "the running job's record must survive"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), drain_frames(stream))
                .await
                .is_err(),
            "the call must still be in flight: running work keeps no timer"
        );
    }

    #[tokio::test]
    async fn begin_tool_call_returns_before_the_job_is_ready() {
        let svc = spawning_service(KubeCalls::default(), None).await;
        let handle = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            svc.begin_tool_call(authed(echo_request())),
        )
        .await
        .expect("BeginToolCall must return its handle before the job is ready — blocking would make startup uncancelable")
        .expect("begin_tool_call must succeed")
        .into_inner();
        assert!(!handle.call_id.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn call_never_attaches_to_an_adopted_record() {
        let calls = KubeCalls::default();
        let svc = spawning_service(calls.clone(), Some(running_job_json("adopted-job"))).await;
        // The reconcile-adopted shape: a healthy job the controller did not spawn
        // and so cannot name by call id.
        svc.state
            .set_active_job(ActiveJob {
                job_name: "adopted-job".into(),
                job_id: String::new(),
                tool_name: "echo".into(),
                workspace: READY_WORKSPACE.into(),
                last_activity: std::time::Instant::now(),
                keepalive_seconds: TOOL_KEEPALIVE_IDLE_SECONDS,
            })
            .await;

        let call_id = svc
            .begin_tool_call(authed(echo_request()))
            .await
            .unwrap()
            .into_inner()
            .call_id;

        assert!(
            calls.deleted_jobs().contains(&"adopted-job".to_string()),
            "a record naming no job id is not attachable: it takes the delete-and-respawn \
             branch so the fresh call stays deadline-bounded; deletes seen: {:?}",
            calls.deleted_jobs()
        );
        assert_eq!(calls.created_jobs(), 1, "the respawn must create one job");
        let record = svc
            .state
            .get_active_job(READY_WORKSPACE, "echo")
            .await
            .expect("the respawn records the fresh job");
        assert_eq!(
            record.job_id, call_id,
            "the record must name the call id that spawned the job"
        );

        let err = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            svc.get_tool_call(authed(GetToolCallRequest {
                job_id: "adopted-job".to_string(),
                tool_name: "echo".to_string(),
            })),
        )
        .await
        .expect("the adopted job phoning home must be refused, never parked")
        .err()
        .expect("a job id that does not match the record is refused");
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    // ---- readyTimeout: prompt path ----

    const PROMPT_PROFILE_KEY: &str = "m1";

    fn test_prompt_profile() -> crate::config::PromptProfile {
        crate::config::PromptProfile {
            image: "ghcr.io/test/prompt:1".into(),
            format: "openai".into(),
            model: "provider/model-1".into(),
            base_url: "https://api.example.test/v1".into(),
            secret: "provider-api-key".into(),
            egress: vec![],
        }
    }

    fn prompt_service(calls: KubeCalls) -> Arc<ControllerService> {
        let state = kube_state(calls, None);
        let mut profiles = std::collections::HashMap::new();
        profiles.insert(PROMPT_PROFILE_KEY.to_string(), test_prompt_profile());
        Arc::new(ControllerService::new(
            state,
            Some(fixed_pair(READY_WORKSPACE)),
            WorkspaceBindings::empty(),
            PromptConfig::from_map(profiles),
        ))
    }

    fn turn_request(model: Option<&str>, conversation_id: &str) -> TurnRequest {
        TurnRequest {
            system: Some("test".into()),
            tools: vec![],
            messages: vec![],
            model: model.map(Into::into),
            reply_channel: None,
            role: None,
            correlation_id: None,
            conversation_id: conversation_id.into(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn prompt_ready_timeout_deletes_the_job_and_resets_state_to_idle() {
        let calls = KubeCalls::default();
        let svc = prompt_service(calls.clone());

        let err = svc
            .turn(authed(turn_request(Some(PROMPT_PROFILE_KEY), "conv-1")))
            .await
            .err()
            .expect("a prompt job that never asks for work must fail the turn");
        assert_eq!(err.code(), tonic::Code::DeadlineExceeded);

        settle().await;
        assert!(
            !calls.deleted_jobs().is_empty(),
            "the expired prompt job must be deleted, not left running as a zombie"
        );
        assert_eq!(
            svc.state.prompt_job_state(PROMPT_PROFILE_KEY).await,
            PromptJobState::Idle,
            "expiry must reset the prompt job state to idle"
        );
    }

    #[tokio::test]
    async fn get_turn_while_prompt_job_state_is_idle_is_refused() {
        let svc = prompt_service(KubeCalls::default());
        svc.state.ensure_model_slot(PROMPT_PROFILE_KEY).await;
        assert_eq!(
            svc.state.prompt_job_state(PROMPT_PROFILE_KEY).await,
            PromptJobState::Idle,
            "precondition"
        );

        let err = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            svc.get_turn(authed(GetTurnRequest {
                model_name: PROMPT_PROFILE_KEY.to_string(),
            })),
        )
        .await
        .expect("an idle slot must refuse GetTurn immediately, never park the caller")
        .err()
        .expect("a job whose readyTimeout already fired cannot re-register itself");
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test(start_paused = true)]
    async fn turn_after_prompt_ready_timeout_launches_a_new_job() {
        let calls = KubeCalls::default();
        let svc = prompt_service(calls.clone());

        let _ = svc
            .turn(authed(turn_request(Some(PROMPT_PROFILE_KEY), "conv-1")))
            .await;
        settle().await;
        assert_eq!(
            calls.created_jobs(),
            1,
            "precondition: the first turn spawns"
        );

        // The expired job phones home late. It must not be able to re-register
        // itself as the slot's connected prompt job.
        let svc_late = svc.clone();
        let late = tokio::spawn(async move {
            svc_late
                .get_turn(authed(GetTurnRequest {
                    model_name: PROMPT_PROFILE_KEY.to_string(),
                }))
                .await
        });
        settle().await;

        let _ = svc
            .turn(authed(turn_request(Some(PROMPT_PROFILE_KEY), "conv-2")))
            .await;
        settle().await;

        assert_eq!(
            calls.created_jobs(),
            2,
            "the turn after an expiry must launch a new job, never silently reuse the zombie"
        );
        late.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn ready_timeout_and_connect_serialize_one_wins() {
        let calls = KubeCalls::default();
        let svc = prompt_service(calls.clone());

        let svc_turn = svc.clone();
        let turning = tokio::spawn(async move {
            svc_turn
                .turn(authed(turn_request(Some(PROMPT_PROFILE_KEY), "conv-1")))
                .await
                .map(|r| r.into_inner())
        });
        // Let the turn create the slot and its job, then park on the ready wait.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let svc_get = svc.clone();
        let connecting = tokio::spawn(async move {
            svc_get
                .get_turn(authed(GetTurnRequest {
                    model_name: PROMPT_PROFILE_KEY.to_string(),
                }))
                .await
        });

        let turn = tokio::time::timeout(std::time::Duration::from_secs(10), turning)
            .await
            .expect("the turn must resolve once its job connects inside readyTimeout")
            .expect("the turn task must not panic");
        assert!(
            turn.is_ok(),
            "the connect won the race, so the expiry must not fire: {:?}",
            turn.err()
        );
        let slot = svc.state.prompt_job_state(PROMPT_PROFILE_KEY).await;
        assert!(
            matches!(slot, PromptJobState::Connected { .. }),
            "the winner is the connect: the slot stays Connected, never reset to Idle, got {slot:?}"
        );

        tokio::time::advance(READY_TIMEOUT * 3).await;
        settle().await;
        assert!(
            calls.deleted_jobs().is_empty(),
            "the loser must do nothing: a connected job is never deleted by the ready deadline; \
             deletes seen: {:?}",
            calls.deleted_jobs()
        );
        connecting.abort();
    }

    // ---- Turn dispatch tests ----

    #[test]
    fn enforce_caller_owns_turn_ok_on_match() {
        assert!(enforce_caller_owns_turn("ws-a", "ws-a", "test_rpc").is_ok());
    }

    #[test]
    fn enforce_caller_owns_turn_denies_on_mismatch() {
        let err = enforce_caller_owns_turn("ws-a", "ws-b", "test_rpc")
            .expect_err("mismatch must return Err");
        assert_eq!(err.code(), tonic::Code::NotFound);
        assert_eq!(err.message(), "turn not found");
    }

    #[tokio::test]
    async fn turn_errors_when_no_verifier_configured() {
        let service = make_service(test_state());

        let status = match service.turn(authed(turn_request(None, "test-conv"))).await {
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

    // ---- drive_turn_result_stream ----

    fn content_delta(text: &str) -> TurnResultChunk {
        TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::ContentDelta(
                toolset_proto::ContentDelta { text: text.into() },
            )),
        }
    }

    fn prompt_job_error_chunk(code: i32, message: &str) -> TurnResultChunk {
        TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Error(toolset_proto::TurnError {
                code,
                message: message.into(),
            })),
        }
    }

    fn terminal_complete() -> TurnResultChunk {
        TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Complete(
                toolset_proto::TurnComplete {
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
            result_tx: TurnResultGuard::new(result_tx),
            workspace: "ws".into(),
            conversation_id: "ws.c".into(),
            reply_channel,
            role: None,
            correlation_id: None,
            system_prompt: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_consumer_frees_prompt_job_instead_of_wedging() {
        let state = test_state();
        let (result_tx, _result_rx) = mpsc::channel::<TurnResultChunk>(2);
        let active = active_turn_with(None, result_tx);

        let stream =
            futures::stream::iter((0..10).map(|i| content_delta(&format!("c{i}")))).map(Ok);

        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            drive_turn_result_stream(&state, stream, active, "m"),
        )
        .await;
        assert!(
            matches!(resp, Ok(Ok(_))),
            "a stalled-but-alive consumer must free the prompt job (Ok), not wedge it: {resp:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_consumer_with_buffered_complete_is_not_left_silent() {
        let state = test_state();
        let (result_tx, mut result_rx) = mpsc::channel::<TurnResultChunk>(1);
        let active = active_turn_with(None, result_tx);

        let stream = futures::stream::iter(vec![Ok(content_delta("a")), Ok(terminal_complete())]);
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            drive_turn_result_stream(&state, stream, active, "m"),
        )
        .await;
        assert!(
            matches!(resp, Ok(Ok(_))),
            "prompt job must be freed: {resp:?}"
        );

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
    async fn prompt_job_error_chunk_forwarded_with_single_terminal() {
        let state = test_state();
        let (result_tx, mut result_rx) = mpsc::channel::<TurnResultChunk>(64);
        let active = active_turn_with(Some("ch".into()), result_tx);

        let stream = futures::stream::iter(vec![Ok(prompt_job_error_chunk(-1, "boom"))]);
        drive_turn_result_stream(&state, stream, active, "m")
            .await
            .unwrap();

        let first = result_rx.recv().await.expect("error chunk forwarded");
        assert!(
            matches!(first.chunk, Some(turn_result_chunk::Chunk::Error(_))),
            "the prompt-job-reported error is forwarded to the workspace stream",
        );
        assert!(
            result_rx.recv().await.is_none(),
            "exactly one terminal — the guard must not append a second",
        );
    }

    #[tokio::test]
    async fn mid_stream_error_still_finishes_cancel_token() {
        let state = test_state();
        state.register_cancel("ws", "ws.c").await;
        assert!(
            state.cancel_token("ws", "ws.c").await.is_some(),
            "precondition"
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
            "a mid-stream transport error from the result stream must surface as Err"
        );
        assert!(
            state.cancel_token("ws", "ws.c").await.is_none(),
            "finish_turn must run on the error path so the token cannot leak",
        );
    }

    #[tokio::test]
    async fn empty_stream_does_not_forward_error_terminal() {
        let state = test_state();
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
}
