use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::info;
use uuid::Uuid;

use toolset_proto::toolset_controller_server::ToolsetController;
use toolset_proto::{
    CancelToolCallRequest, CancelToolCallResponse, Grant, ReportDiscoveredToolsAck,
    ReportDiscoveredToolsRequest, Tool, ToolCallHandle, ToolList, ToolsetGrantNames,
};

#[cfg(test)]
use proto_common::tool_result_frame::Frame;
use proto_common::{AwaitToolResultRequest, CallToolRequest, ToolResultFrame, WatchToolsRequest};

use crate::audience_layer::RequiredAudience;
use crate::job;
use crate::keepalive::TOOL_KEEPALIVE_IDLE_SECONDS;
use crate::registry::{ArgDecl, ArgType};
use crate::state::{
    ActiveJob, CapabilityGrant, ControllerState, PendingCall, RecordEviction, RegisteredTool,
    WorkspaceBindings, RESULT_CHANNEL_CAPACITY,
};
use crate::validation::synthesize_schema;
use crate::WORKSPACE_MOUNT_PATH;
use shared::auth::{extract_bearer_token, TokenVerifier};
use shared::keepalive::{delete_job, job_health, JobHealth, STARTUP_GRACE};
use shared::toolset::validate_call_input;

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
}

impl ControllerService {
    pub fn new(
        state: Arc<ControllerState>,
        verifiers: Option<VerifierPair>,
        bindings: WorkspaceBindings,
    ) -> Self {
        Self {
            state,
            verifiers,
            bindings,
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
    /// configured. Used where per-workspace ownership is enforced and so cannot
    /// proceed without an identity.
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

/// A grant a call selected: the name as stored in the binding, and the grant.
type ResolvedGrant<'a> = (&'a str, &'a CapabilityGrant);

/// Split a tool call's input into the JSON its declared arguments are validated
/// against and the grant the reserved `__grant` key selects.
///
/// `__grant` is compared as an exact member of the grants bound for this
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
        .and_then(|grants| grants.get_key_value(name.as_str()))
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
) -> Vec<Tool> {
    let raw = match workspace {
        Some(ws) => state.list_tools_for_workspace(ws, bindings).await,
        None => state.list_tools().await,
    };
    raw.into_iter()
        .map(|(name, tool)| Tool {
            name,
            description: tool.description,
            parameters_json: synthesize_schema(&tool.args),
            toolset: tool.toolset_name,
            args: tool.args.iter().map(|a| a.to_tool_arg()).collect(),
        })
        .collect()
}

/// Map a workspace's bound grants onto the wire. Each bound toolset that
/// carries grants contributes one row naming them. Grant names only: the
/// harness mounts the same bindings ConfigMap and resolves each name's Secret
/// and mount path itself, so no credential detail leaves the controller. The
/// workspace-less snapshot carries no grants, since a grant is only selectable
/// against a binding.
fn snapshot_grants_for(
    workspace: Option<&str>,
    bindings: &WorkspaceBindings,
) -> Vec<ToolsetGrantNames> {
    let Some(ws) = workspace else {
        return vec![];
    };
    bindings
        .toolsets_for(ws)
        .iter()
        .filter_map(|entry| {
            let bound = entry.grants()?;
            Some(ToolsetGrantNames {
                toolset: entry.name().to_string(),
                grants: bound
                    .keys()
                    .map(|name| Grant { name: name.clone() })
                    .collect(),
            })
        })
        .collect()
}

#[tonic::async_trait]
impl ToolsetController for ControllerService {
    // =====================================================================
    // Tool dispatch
    // =====================================================================

    type WatchToolsStream = Pin<Box<dyn Stream<Item = Result<ToolList, Status>> + Send + 'static>>;

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
        let (tx, rx) = mpsc::channel::<Result<ToolList, Status>>(8);

        tokio::spawn(async move {
            loop {
                let tools = snapshot_tools_for(&state, workspace.as_deref(), &bindings).await;
                let grants = snapshot_grants_for(workspace.as_deref(), &bindings);
                if tx.send(Ok(ToolList { tools, grants })).await.is_err() {
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

        // The job this call may run on, resolved under the dispatch lock.
        // Empty names no job, so the call is claimed by nobody.
        let mut target_job_id = String::new();
        let mut target_job_name = String::new();

        // The grant name this call carries, keying its pending and active slots
        // so distinct grants for one tool never share a job.
        let call_grant = grant.as_ref().map(|(name, _)| name.to_string());

        // Per-tool dispatch mutex held only across the get-probe-create-set
        // sequence so concurrent calls for the same tool cannot both spawn.
        {
            let dispatch_lock = self.state.tool_dispatch_lock(&workspace, tool_name).await;
            let _dispatch_guard = dispatch_lock.lock().await;

            if let Some(client) = self.state.kube_client() {
                let workspace_pvc = format!("workspace-data-{}", workspace);
                needs_deadline = true;

                let should_spawn = match self
                    .state
                    .get_active_job(&workspace, tool_name, call_grant.as_deref())
                    .await
                {
                    None => true,
                    // A record naming no job id was adopted at reconcile: no pod
                    // can be matched against it, so it is not attachable and
                    // takes the delete-and-respawn branch.
                    Some(active) if active.job_id.is_empty() => {
                        info!(
                            tool = %tool_name,
                            workspace = %workspace,
                            adopted_job = %active.job_name,
                            "adopted ActiveJob names no call id; deleting + recreating"
                        );
                        self.state
                            .remove_active_job(&workspace, tool_name, call_grant.as_deref())
                            .await;
                        self.state
                            .retire_calls_for_tool_job(&workspace, tool_name)
                            .await;
                        delete_job_logged(client, self.state.namespace(), &active.job_name).await;
                        true
                    }
                    Some(active) => {
                        let health =
                            job_health(client, self.state.namespace(), &active.job_name).await;
                        match health {
                            JobHealth::Running => {
                                needs_deadline = false;
                                target_job_id = active.job_id.clone();
                                target_job_name = active.job_name.clone();
                                false
                            }
                            JobHealth::Pending { age } if age < STARTUP_GRACE => {
                                target_job_id = active.job_id.clone();
                                target_job_name = active.job_name.clone();
                                false
                            }
                            JobHealth::Pending { .. } | JobHealth::Failed | JobHealth::NotFound => {
                                info!(
                                    tool = %tool_name,
                                    workspace = %workspace,
                                    stale_job = %active.job_name,
                                    health = ?health,
                                    "stale ActiveJob entry; deleting + recreating"
                                );
                                self.state
                                    .remove_active_job(&workspace, tool_name, call_grant.as_deref())
                                    .await;
                                self.state
                                    .retire_calls_for_tool_job(&workspace, tool_name)
                                    .await;
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
                    target_job_name = job_name.clone();
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
                            grant: call_grant.clone(),
                        })
                        .await;
                    target_job_id = call_id.clone();
                }
            } else {
                // No kube client: nothing can be spawned, so the call rides
                // whatever job the record already names.
                target_job_id = self
                    .state
                    .get_active_job(&workspace, tool_name, call_grant.as_deref())
                    .await
                    .map(|active| active.job_id)
                    .unwrap_or_default();
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
                grant: call_grant.clone(),
                target_job_id: target_job_id.clone(),
            })
            .await;

        info!(call_id = %call_id, tool = %tool_name, "call enqueued");

        // The publish above runs unlocked, so the job can be retired under it.
        // `remove_pending_call` arbitrates: losing it means a job took the call.
        let retired = target_job_id.is_empty()
            || self
                .state
                .get_active_job(&workspace, tool_name, call_grant.as_deref())
                .await
                .is_none_or(|active| active.job_id != target_job_id);
        if retired
            && self
                .state
                .remove_pending_call(&workspace, tool_name, call_grant.as_deref(), &call_id)
                .await
        {
            self.state.finish_call(&call_id).await;
            drop(self.state.take_result_tx(&call_id).await);
            return Err(Status::unavailable(format!(
                "the tool job for {tool_name} was retired before the call reached it"
            )));
        }

        if needs_deadline {
            self.arm_ready_deadline(
                workspace,
                tool_name.clone(),
                call_grant,
                call_id.clone(),
                target_job_name,
            );
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
    fn arm_ready_deadline(
        &self,
        workspace: String,
        tool_name: String,
        grant: Option<String>,
        call_id: String,
        job_name: String,
    ) {
        // The bound runs from the moment the call is enqueued, not from whenever
        // the executor first polls the task.
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        let state = self.state.clone();
        tokio::spawn(async move {
            // Decided under the dispatch lock. Reading the slot here would name
            // whatever holds it by now, and kill a job this call never rode.
            let job_name = Some(job_name).filter(|n| !n.is_empty());

            tokio::time::sleep_until(deadline).await;
            if !state
                .remove_pending_call(&workspace, &tool_name, grant.as_deref(), &call_id)
                .await
            {
                return;
            }
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

            if let Some(job_name) = job_name {
                // Retire the job only while the slot still holds it; a
                // successor belongs to a later call and its own deadline.
                if state
                    .remove_active_job_named(&workspace, &tool_name, &job_name)
                    .await
                    != RecordEviction::Removed
                {
                    return;
                }
                state
                    .retire_calls_for_tool_job(&workspace, &tool_name)
                    .await;
                if let Some(client) = state.kube_client() {
                    delete_job_logged(client, state.namespace(), &job_name).await;
                }
            }
        });
    }

    /// Forward a runtime's inbound frame stream to the call's parked
    /// `AwaitToolResult` server-stream, then retire the call. Retained for its
    /// forward/terminal/cleanup coverage now that no controller RPC drives the
    /// tool-dispatch pull path; exercised only by the tests below.
    #[cfg(test)]
    async fn forward_result_frames<S>(&self, call_id: String, mut stream: S) -> Result<(), Status>
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

        Ok(())
    }
}

/// Bound on every wait for a job to become ready, where ready means the job has
/// connected and asked for work — not pod scheduled, not Job created. Running
/// work carries no time bound. A baked default now; a per-toolset-image config
/// key later.
pub const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ToolsetEntry;
    use crate::registry::{ArgDecl, ArgType};
    use crate::state::RegisteredTool;
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
        ControllerService::new(state, None, WorkspaceBindings::empty())
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
                grant: None,
            })
            .await;
        Arc::new(ControllerService::new(
            state,
            Some(fixed_pair(READY_WORKSPACE)),
            bindings,
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

        let assignment = svc
            .state
            .dequeue_call(READY_WORKSPACE, "echo", "job-1")
            .await
            .expect("the seeded job claims its enqueued call");

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
                grant: None,
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
            .get_active_job(READY_WORKSPACE, "echo", None)
            .await
            .expect("the ActiveJob must still exist after forwarding");
        assert!(
            job.last_activity > stale,
            "completing the result stream must bump the tool's last_activity"
        );
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
    async fn watch_tools_names_each_tools_toolset() {
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
        assert_eq!(
            first.tools[0].toolset, "c1",
            "the snapshot must say which toolset each tool belongs to"
        );
    }

    #[tokio::test]
    async fn watch_tools_carries_each_args_env_mapping() {
        use futures::StreamExt;
        let state = test_state();
        register_tool_with_args(
            &state,
            "git",
            "commit",
            "record a commit",
            vec![arg("message", ArgType::String, true, "MESSAGE")],
        )
        .await;

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
        assert_eq!(first.tools[0].args.len(), 1);
        assert_eq!(
            first.tools[0].args[0].env, "MESSAGE",
            "the harness-facing snapshot must carry each arg's env mapping"
        );
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

        let svc = ControllerService::new(state, Some(fixed_pair("alpha")), bindings);

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
    async fn await_tool_result_rejects_missing_token() {
        // With a verifier configured, the harness-audience stamp is enforced via
        // verify_workspace_required. A request carrying no bearer token must be
        // rejected before the handler consumes the result receiver.
        let svc = ControllerService::new(
            test_state(),
            Some(fixed_pair("ws")),
            WorkspaceBindings::empty(),
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
        // A call with no job to ride is refused at publish, so the owner needs a
        // job to serve `echo` before it can hold a call for the intruder to try.
        state
            .set_active_job(ActiveJob {
                job_name: "job-1".into(),
                job_id: "job-1".into(),
                tool_name: "echo".into(),
                workspace: READY_WORKSPACE.into(),
                last_activity: std::time::Instant::now(),
                keepalive_seconds: TOOL_KEEPALIVE_IDLE_SECONDS,
                grant: None,
            })
            .await;
        let owner = Arc::new(ControllerService::new(
            state.clone(),
            Some(fixed_pair(READY_WORKSPACE)),
            bindings.clone(),
        ));
        let intruder = Arc::new(ControllerService::new(
            state,
            Some(fixed_pair(INTRUDER_WORKSPACE)),
            bindings,
        ));
        (owner, intruder)
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
        mock_kube_client_with_delete(calls, job_get, true)
    }

    /// `mock_kube_client`, with the DELETE outcome selectable so a caller can
    /// drive the branch where the API server refuses to retire a Job.
    fn mock_kube_client_with_delete(
        calls: KubeCalls,
        job_get: Option<serde_json::Value>,
        delete_succeeds: bool,
    ) -> kube::Client {
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
                    http::Method::DELETE if !delete_succeeds => (
                        500,
                        br#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":500,"reason":"InternalError","message":"delete refused"}"#.to_vec(),
                    ),
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

        // No job ever asks for work.
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
            .get_active_job(READY_WORKSPACE, "echo", None)
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
                .get_active_job(READY_WORKSPACE, "echo", None)
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
                .get_active_job(READY_WORKSPACE, "echo", None)
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
                grant: None,
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
                .get_active_job(READY_WORKSPACE, "echo", None)
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

    // ---- Grant change on a live keepalive job ----
    //
    // A keepalive pod holds exactly the credential it was spawned with, plus
    // the grant label its egress policy selects on. A call selecting a
    // different grant spawns its own slot beside the live pod, keyed by that
    // grant. It never retires or rides the other grant's pod.

    /// Bindings whose one toolset carries two grants, loaded through the real
    /// loader (the in-memory constructor builds bare entries only).
    fn two_grant_bindings() -> WorkspaceBindings {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("grant-swap-{}-{}.yaml", std::process::id(), seq));
        std::fs::write(
            &path,
            format!(
                "{READY_WORKSPACE}:\n  - name: test-toolset\n    grants:\n      \
                 alpha:\n        secret: s-alpha\n      beta:\n        secret: s-beta\n"
            ),
        )
        .expect("write temp bindings");
        let loaded = WorkspaceBindings::load(path.to_str().expect("utf-8 temp path"));
        let _ = std::fs::remove_file(&path);
        loaded.expect("grant-bearing bindings must load")
    }

    /// A service whose workspace binds `test-toolset` with two grants, holding
    /// a live `alpha`-granted job the health probe reports as running.
    async fn granted_service(calls: KubeCalls, delete_succeeds: bool) -> Arc<ControllerService> {
        let state = ControllerState::new(
            Some(mock_kube_client_with_delete(
                calls,
                Some(running_job_json("job-alpha")),
                delete_succeeds,
            )),
            "test-ns".into(),
            "http://toolset-ctrl:9090".into(),
            shared::scheduling::SchedulingConfig::default(),
        );
        echo_state(&state, &[]).await;
        state
            .set_active_job(ActiveJob {
                job_name: "job-alpha".into(),
                job_id: "call-alpha".into(),
                tool_name: "echo".into(),
                workspace: READY_WORKSPACE.into(),
                last_activity: std::time::Instant::now(),
                keepalive_seconds: TOOL_KEEPALIVE_IDLE_SECONDS,
                grant: Some("alpha".into()),
            })
            .await;
        Arc::new(ControllerService::new(
            state,
            Some(fixed_pair(READY_WORKSPACE)),
            two_grant_bindings(),
        ))
    }

    fn granted_call(grant: &str) -> CallToolRequest {
        CallToolRequest {
            name: "echo".to_string(),
            input_json: format!(r#"{{"message":"hello","__grant":"{grant}"}}"#),
            conversation_id: String::new(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_selecting_another_grant_spawns_its_own_slot_beside_the_live_job() {
        let calls = KubeCalls::default();
        let svc = granted_service(calls.clone(), true).await;

        let call_id = svc
            .begin_tool_call(authed(granted_call("beta")))
            .await
            .expect("the call is admitted")
            .into_inner()
            .call_id;

        // Materiality: the live job is healthy and its record names a call id,
        // so without the grant in the key this call ATTACHES to the alpha pod —
        // no create — and the beta credential never reaches a pod. With the
        // grant keyed, beta spawns its own pod and alpha is left alone.
        assert!(
            calls.deleted_jobs().is_empty(),
            "coexisting grants must not retire each other, deletes seen: {:?}",
            calls.deleted_jobs()
        );
        assert_eq!(calls.created_jobs(), 1, "beta spawns exactly one pod");

        let alpha = svc
            .state
            .get_active_job(READY_WORKSPACE, "echo", Some("alpha"))
            .await
            .expect("the alpha slot survives");
        assert_eq!(alpha.job_name, "job-alpha", "alpha's job is untouched");
        assert_eq!(alpha.job_id, "call-alpha", "alpha's call id is untouched");

        let beta = svc
            .state
            .get_active_job(READY_WORKSPACE, "echo", Some("beta"))
            .await
            .expect("beta gets its own slot");
        assert_eq!(
            beta.job_id, call_id,
            "beta's slot names the call that spawned its own pod"
        );

        assert_eq!(
            svc.state.active_job_count().await,
            2,
            "alpha and beta occupy distinct slots"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_selecting_the_same_grant_rides_the_live_job() {
        let calls = KubeCalls::default();
        let svc = granted_service(calls.clone(), true).await;

        svc.begin_tool_call(authed(granted_call("alpha")))
            .await
            .expect("the call is admitted");

        // The keep arm: retiring on every grant-bearing call would pass the
        // test above while destroying keepalive.
        assert!(
            calls.deleted_jobs().is_empty(),
            "a matching grant must not retire the live job, deletes seen: {:?}",
            calls.deleted_jobs()
        );
        assert_eq!(
            calls.created_jobs(),
            0,
            "a matching grant must attach, not respawn"
        );
    }

    /// Retiring a job without draining strands anything already riding it: the
    /// call names a job no record holds, so no pod can claim it, and an attach
    /// carries no deadline. Breaks if a replace branch drops its drain.
    #[tokio::test(start_paused = true)]
    async fn replacing_a_job_terminates_the_calls_riding_it() {
        let calls = KubeCalls::default();
        let svc = spawning_service(calls.clone(), Some(running_job_json("adopted-job"))).await;
        svc.state
            .set_active_job(ActiveJob {
                job_name: "adopted-job".into(),
                job_id: String::new(),
                tool_name: "echo".into(),
                workspace: READY_WORKSPACE.into(),
                last_activity: std::time::Instant::now(),
                keepalive_seconds: TOOL_KEEPALIVE_IDLE_SECONDS,
                grant: None,
            })
            .await;

        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        svc.state
            .set_result_tx(
                "riding-call".into(),
                READY_WORKSPACE.into(),
                "echo".into(),
                tx,
            )
            .await;

        svc.begin_tool_call(authed(echo_request()))
            .await
            .expect("the replacement spawns");

        let frame = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("the riding call must be terminated, not stranded");
        match frame.and_then(|f| f.frame) {
            Some(Frame::Complete(c)) => {
                assert_ne!(c.outcome(), ToolOutcome::Done, "terminal must be an error")
            }
            other => panic!("expected an error ToolComplete terminal, got {other:?}"),
        }
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
                grant: None,
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
            .get_active_job(READY_WORKSPACE, "echo", None)
            .await
            .expect("the respawn records the fresh job");
        assert_eq!(
            record.job_id, call_id,
            "the record must name the call id that spawned the job"
        );
    }
}
