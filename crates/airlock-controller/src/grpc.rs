use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::info;
use uuid::Uuid;

use airlock_proto::airlock_controller_server::AirlockController;
use airlock_proto::{
    AwaitToolCancelRequest, AwaitToolResultRequest, CancelToolCallRequest, CancelToolCallResponse,
    GetToolCallRequest, SendToolResultAck, SendToolResultRequest, ToolCallAssignment,
    ToolCallHandle, ToolCancelSignal,
};
use proto_common::{
    CallToolRequest, CallToolResponse, ToolInfo, ToolListUpdate, WatchToolsRequest,
};

use crate::job;
use crate::keepalive::KEEPALIVE_IDLE_SECONDS;
use crate::state::{ActiveJob, ControllerState, PendingCall, ToolCallResult, WorkspaceBindings};
use crate::validation::{synthesize_schema, validate_call_input};
use crate::WORKSPACE_MOUNT_PATH;
use shared::auth::{extract_bearer_token, TokenVerifier};
use shared::keepalive::{delete_job, job_health, JobHealth, STARTUP_GRACE};

pub struct ControllerService {
    state: Arc<ControllerState>,
    verifier: Option<Arc<dyn TokenVerifier>>,
    bindings: WorkspaceBindings,
}

impl ControllerService {
    pub fn new(
        state: Arc<ControllerState>,
        verifier: Option<Arc<dyn TokenVerifier>>,
        bindings: WorkspaceBindings,
    ) -> Self {
        Self {
            state,
            verifier,
            bindings,
        }
    }

    /// Resolve the calling workspace from a gRPC request's bearer token,
    /// returning `None` when the verifier is not configured (the no-auth
    /// development path).
    async fn verify_workspace<T>(&self, request: &Request<T>) -> Result<Option<String>, Status> {
        match &self.verifier {
            Some(verifier) => {
                let token = extract_bearer_token(request)?;
                Ok(Some(verifier.verify_token(token).await?))
            }
            None => Ok(None),
        }
    }
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
impl AirlockController for ControllerService {
    type WatchToolsStream =
        Pin<Box<dyn Stream<Item = Result<ToolListUpdate, Status>> + Send + 'static>>;

    async fn watch_tools(
        &self,
        request: Request<WatchToolsRequest>,
    ) -> Result<Response<Self::WatchToolsStream>, Status> {
        let workspace = self.verify_workspace(&request).await?;

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
        let workspace_name = self.verify_workspace(&request).await?;

        let req = request.into_inner();
        let tool_name = &req.name;

        let tool = self
            .state
            .get_tool(tool_name)
            .await
            .ok_or_else(|| Status::not_found(format!("unknown tool: {tool_name}")))?;

        if let Some(ref workspace) = workspace_name {
            if !self.bindings.has_chamber(workspace, &tool.chamber_name) {
                return Err(Status::permission_denied(format!(
                    "workspace {workspace} is not authorized for chamber {}",
                    tool.chamber_name
                )));
            }
        }

        let args = validate_call_input(&req.input_json, &tool.args)?;

        let chamber = self
            .state
            .get_chamber(&tool.chamber_name)
            .await
            .ok_or_else(|| {
                Status::failed_precondition(format!("chamber {} not found", tool.chamber_name))
            })?;

        let call_id = Uuid::new_v4().to_string();
        let working_dir = WORKSPACE_MOUNT_PATH.to_string();

        // Per-tool dispatch mutex held only across the get-probe-create-set
        // sequence; the surrounding block scopes the guard so it drops
        // before enqueue/result_rx and concurrent calls for the same tool
        // can queue in parallel.
        {
            let dispatch_lock = self.state.tool_dispatch_lock(tool_name).await;
            let _dispatch_guard = dispatch_lock.lock().await;

            if let Some(client) = self.state.kube_client() {
                let workspace = workspace_name
                    .as_deref()
                    .expect("kube_client present implies verifier present");
                let workspace_pvc = format!("{}-workspace-data", workspace);

                // Dedup: if we have an ActiveJob for this tool and k8s
                // confirms it's healthy, skip the spawn. The state map is a
                // cache — every reuse decision is backed by a live kube
                // probe, so stale entries (out-of-band delete, crashloop,
                // image-pull-backoff past grace) recreate.
                let should_spawn = match self.state.get_active_job(tool_name).await {
                    None => true,
                    Some(active) => {
                        let health =
                            job_health(client, self.state.namespace(), &active.job_name).await;
                        match health {
                            JobHealth::Running => false,
                            JobHealth::Pending { age } if age < STARTUP_GRACE => false,
                            JobHealth::Pending { .. } | JobHealth::Failed | JobHealth::NotFound => {
                                info!(
                                    tool = %tool_name,
                                    stale_job = %active.job_name,
                                    health = ?health,
                                    "stale ActiveJob entry; deleting + recreating"
                                );
                                self.state.remove_active_job(tool_name).await;
                                // Best-effort; NotFound is fine, transient
                                // errors retry on the next CallTool.
                                let _ =
                                    delete_job(client, self.state.namespace(), &active.job_name)
                                        .await;
                                true
                            }
                        }
                    }
                };

                if should_spawn {
                    let job_spec = job::build_tool_job(
                        tool_name,
                        &tool.image,
                        &tool.chamber_name,
                        &chamber.spec,
                        &call_id,
                        self.state.namespace(),
                        self.state.controller_addr(),
                        workspace,
                        &workspace_pvc,
                        self.state.scheduling(),
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
                    // Register the spawn in `active_jobs` so the next call
                    // dedups, and so the cleanup loop can reap it after
                    // idle. Fire-and-forget Jobs (`keepalive=false`) carry
                    // `keepalive_seconds=0` which `find_expired_jobs`
                    // explicitly excludes.
                    self.state
                        .set_active_job(
                            tool_name.clone(),
                            ActiveJob {
                                job_name,
                                tool_name: tool_name.clone(),
                                last_activity: std::time::Instant::now(),
                                keepalive_seconds: if chamber.spec.keepalive {
                                    KEEPALIVE_IDLE_SECONDS
                                } else {
                                    0
                                },
                            },
                        )
                        .await;
                }
            }
        }

        let (result_tx, result_rx) = oneshot::channel::<ToolCallResult>();

        self.state
            .set_result_tx(call_id.clone(), tool_name.clone(), result_tx)
            .await;
        self.state.set_result_rx(call_id.clone(), result_rx).await;
        self.state.register_cancel(call_id.clone()).await;

        self.state
            .enqueue_call(PendingCall {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                args,
                working_dir,
            })
            .await;

        info!(call_id = %call_id, tool = %tool_name, "call enqueued");

        Ok(Response::new(ToolCallHandle { call_id }))
    }

    async fn await_tool_result(
        &self,
        request: Request<AwaitToolResultRequest>,
    ) -> Result<Response<CallToolResponse>, Status> {
        let call_id = request.into_inner().call_id;

        let result_rx = self.state.take_result_rx(&call_id).await.ok_or_else(|| {
            Status::not_found(format!("no in-flight call for call_id: {call_id}"))
        })?;

        info!(call_id = %call_id, "awaiting call result");

        let result = result_rx
            .await
            .map_err(|_| Status::internal(format!("result channel dropped for call {call_id}")));
        self.state.finish_call(&call_id).await;
        let result = result?;

        Ok(Response::new(CallToolResponse {
            output: result.output,
            is_error: result.is_error,
        }))
    }

    async fn cancel_tool_call(
        &self,
        request: Request<CancelToolCallRequest>,
    ) -> Result<Response<CancelToolCallResponse>, Status> {
        let call_id = request.into_inner().call_id;
        if call_id.is_empty() {
            return Err(Status::invalid_argument("call_id must not be empty"));
        }

        let cancelled = self.state.fire_cancel(&call_id).await;
        info!(call_id = %call_id, cancelled, "cancel requested");

        Ok(Response::new(CancelToolCallResponse { cancelled }))
    }

    async fn await_tool_cancel(
        &self,
        request: Request<AwaitToolCancelRequest>,
    ) -> Result<Response<ToolCancelSignal>, Status> {
        let call_id = request.into_inner().call_id;

        // Unknown/finished id: bare return, which the runtime reads as "no
        // cancel". Otherwise block on a clone of the call's token until a
        // `cancel_tool_call` fires it.
        if let Some(token) = self.state.cancel_token(&call_id).await {
            token.cancelled().await;
        }

        Ok(Response::new(ToolCancelSignal {}))
    }

    async fn get_tool_call(
        &self,
        request: Request<GetToolCallRequest>,
    ) -> Result<Response<ToolCallAssignment>, Status> {
        let req = request.into_inner();
        let tool_name = &req.tool_name;

        loop {
            if let Some(call) = self.state.dequeue_call(tool_name).await {
                info!(
                    call_id = %call.call_id,
                    job_id = %req.job_id,
                    tool = %tool_name,
                    "dispatching call to runtime"
                );
                return Ok(Response::new(ToolCallAssignment {
                    call_id: call.call_id,
                    working_dir: call.working_dir,
                    args: call.args,
                }));
            }

            self.state.wait_for_call().await;
        }
    }

    async fn send_tool_result(
        &self,
        request: Request<SendToolResultRequest>,
    ) -> Result<Response<SendToolResultAck>, Status> {
        let req = request.into_inner();

        let (tx, tool_name) = self
            .state
            .take_result_tx(&req.call_id)
            .await
            .ok_or_else(|| {
                Status::not_found(format!("no pending result for call_id: {}", req.call_id))
            })?;

        info!(
            call_id = %req.call_id,
            exit_code = req.exit_code,
            "received tool result"
        );

        // Bump idle timer on the tool's ActiveJob now that the runtime
        // is back to polling. Truthful "back to idle" signal — beats
        // bumping on enqueue (would inflate idleness during quiet
        // periods after a long-stuck call) and bumping on dispatch
        // (would swallow hangs into "still active").
        if !tool_name.is_empty() {
            self.state.bump_last_activity(&tool_name).await;
        }

        let _ = tx.send(ToolCallResult {
            output: req.output,
            is_error: req.is_error,
            exit_code: req.exit_code,
        });

        Ok(Response::new(SendToolResultAck {}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{Chamber, ChamberSpec};
    use crate::registry::{ArgDecl, ArgType};
    use crate::state::RegisteredTool;
    use shared::auth::TokenVerifier;

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
        chamber: &str,
        name: &str,
        desc: &str,
        args: Vec<ArgDecl>,
    ) {
        state
            .set_tools_for_chamber(
                chamber,
                vec![RegisteredTool {
                    name: name.to_string(),
                    chamber_name: chamber.to_string(),
                    description: desc.to_string(),
                    image: "test:latest".to_string(),
                    args,
                }],
            )
            .await;
    }

    struct MockTokenVerifier(String);

    #[async_trait::async_trait]
    impl TokenVerifier for MockTokenVerifier {
        async fn verify_token(&self, _token: &str) -> Result<String, Status> {
            Ok(self.0.clone())
        }
    }

    fn make_chamber(name: &str) -> Chamber {
        Chamber::new(
            name,
            ChamberSpec {
                image: None,
                credentials: vec![],
                egress: vec![],
                keepalive: false,
            },
        )
    }

    fn make_service(state: Arc<ControllerState>) -> ControllerService {
        ControllerService::new(state, None, WorkspaceBindings::empty())
    }

    async fn register_tools(state: &ControllerState, chamber: &str, tools: Vec<(&str, &str)>) {
        let registered: Vec<RegisteredTool> = tools
            .into_iter()
            .map(|(name, desc)| RegisteredTool {
                name: name.to_string(),
                chamber_name: chamber.to_string(),
                description: desc.to_string(),
                image: "test:latest".to_string(),
                args: vec![],
            })
            .collect();
        state.set_tools_for_chamber(chamber, registered).await;
    }

    #[tokio::test]
    async fn call_tool_unknown_returns_not_found() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        let svc = make_service(state);
        let err = svc
            .begin_tool_call(Request::new(CallToolRequest {
                name: "nonexistent".to_string(),
                input_json: "{}".to_string(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn call_tool_missing_chamber_returns_failed_precondition() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        register_tools(&state, "test-chamber", vec![("echo", "Echo tool")]).await;

        let svc = make_service(state);
        let err = svc
            .begin_tool_call(Request::new(CallToolRequest {
                name: "echo".to_string(),
                input_json: "{}".to_string(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn call_tool_round_trip() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        register_tool_with_args(
            &state,
            "test-chamber",
            "echo",
            "Echo tool",
            vec![arg("message", ArgType::String, true, "MESSAGE")],
        )
        .await;
        state
            .set_chamber("test-chamber".into(), make_chamber("test-chamber"))
            .await;

        let svc = Arc::new(make_service(state.clone()));

        let handle = svc
            .begin_tool_call(Request::new(CallToolRequest {
                name: "echo".to_string(),
                input_json: r#"{"message":"hello"}"#.to_string(),
            }))
            .await
            .expect("begin_tool_call must not block on the result")
            .into_inner();

        let assignment = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            svc.get_tool_call(Request::new(GetToolCallRequest {
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

        svc.send_tool_result(Request::new(SendToolResultRequest {
            call_id: assignment.call_id,
            output: "hello\n".to_string(),
            is_error: false,
            exit_code: 0,
        }))
        .await
        .unwrap();

        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            svc.await_tool_result(Request::new(AwaitToolResultRequest {
                call_id: handle.call_id,
            })),
        )
        .await
        .expect("await_tool_result timed out")
        .unwrap()
        .into_inner();
        assert_eq!(resp.output, "hello\n");
        assert!(!resp.is_error);
    }

    #[tokio::test]
    async fn get_tool_call_blocks_until_enqueued() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        register_tool_with_args(
            &state,
            "test-chamber",
            "tool",
            "test tool",
            vec![arg("x", ArgType::String, true, "X")],
        )
        .await;
        state
            .set_chamber("test-chamber".into(), make_chamber("test-chamber"))
            .await;

        let svc = Arc::new(make_service(state.clone()));

        let svc_for_get = svc.clone();
        let get_handle = tokio::spawn(async move {
            svc_for_get
                .get_tool_call(Request::new(GetToolCallRequest {
                    job_id: "job-1".to_string(),
                    tool_name: "tool".to_string(),
                }))
                .await
        });

        tokio::task::yield_now().await;
        assert!(!get_handle.is_finished(), "GetToolCall should be blocking");

        let svc_for_call = svc.clone();
        tokio::spawn(async move {
            let _ = svc_for_call
                .begin_tool_call(Request::new(CallToolRequest {
                    name: "tool".to_string(),
                    input_json: r#"{"x":"test"}"#.to_string(),
                }))
                .await;
        });

        let assignment = tokio::time::timeout(std::time::Duration::from_secs(2), get_handle)
            .await
            .expect("GetToolCall should resolve within timeout")
            .unwrap()
            .unwrap()
            .into_inner();

        assert_eq!(assignment.args.get("X"), Some(&"test".to_string()));
    }

    #[tokio::test]
    async fn send_result_unknown_call_id() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        let svc = make_service(state);
        let err = svc
            .send_tool_result(Request::new(SendToolResultRequest {
                call_id: "nonexistent".to_string(),
                output: "".to_string(),
                is_error: false,
                exit_code: 0,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn watch_tools_emits_initial_snapshot() {
        use futures::StreamExt;

        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
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
    async fn watch_tools_emits_update_on_chamber_change() {
        use futures::StreamExt;

        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        let svc = make_service(state.clone());
        let resp = svc
            .watch_tools(Request::new(WatchToolsRequest {}))
            .await
            .unwrap();
        let mut stream = resp.into_inner();

        // Initial snapshot: empty.
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(first.tools.is_empty());

        // Mutate state — handler must push a fresh snapshot.
        register_tools(&state, "c1", vec![("git", "push commits")]).await;

        let second = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("watch_tools must push update after set_tools_for_chamber")
            .expect("stream not closed")
            .expect("ok response");
        assert_eq!(second.tools.len(), 1);
        assert_eq!(second.tools[0].name, "git");
    }

    #[tokio::test]
    async fn call_tool_unauthorized_chamber_returns_permission_denied() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        register_tools(&state, "git", vec![("git-push", "Push commits")]).await;
        state.set_chamber("git".into(), make_chamber("git")).await;

        let mut bindings_map = std::collections::HashMap::new();
        bindings_map.insert("alpha".to_string(), vec!["ssh".to_string()]);
        let bindings = WorkspaceBindings::from_map(bindings_map);

        let verifier: Arc<dyn TokenVerifier> = Arc::new(MockTokenVerifier("alpha".to_string()));
        let svc = ControllerService::new(state, Some(verifier), bindings);

        let mut request = Request::new(CallToolRequest {
            name: "git-push".to_string(),
            input_json: "{}".to_string(),
        });
        request
            .metadata_mut()
            .insert("authorization", "Bearer fake-token".parse().unwrap());

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            svc.begin_tool_call(request),
        )
        .await
        .expect("begin_tool_call should reject immediately, not block");
        assert_eq!(result.unwrap_err().code(), tonic::Code::PermissionDenied);
    }

    // ---- ACCEPTANCE (turn-cancel-cascade-chambers) ----
    //
    // Spec: ~/vault/projects/sycophant/specs/turn-cancel-cascade-chambers/spec.md
    //
    // These pin the controller half of the cascade: the begin/await split
    // surfaces a call_id before the result (AC-1), an unknown/finished cancel is
    // a safe no-op that leaves bookkeeping intact (AC-4), a killed SendToolResult
    // unblocks the parked awaiter (AC-5), and the runtime-facing AwaitToolCancel
    // long-poll returns once a cancel fires (delivery seam for AC-3).
    //
    // Expected new surface (does not exist yet — these are red on the missing
    // begin/await/cancel plumbing, not on setup):
    //   ControllerService::begin_tool_call(CallToolRequest) -> ToolCallHandle{call_id}
    //   ControllerService::await_tool_result(AwaitToolResultRequest{call_id}) -> CallToolResponse
    //   ControllerService::cancel_tool_call(CancelToolCallRequest{call_id}) -> CancelToolCallResponse{cancelled}
    //   ControllerService::await_tool_cancel(AwaitToolCancelRequest{call_id}) -> ToolCancelSignal

    /// A service with `echo` registered and its chamber present, ready for a
    /// `begin_tool_call` that dispatches without a live k8s (kube_client: None).
    async fn ready_service() -> Arc<ControllerService> {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        register_tool_with_args(
            &state,
            "test-chamber",
            "echo",
            "Echo tool",
            vec![arg("message", ArgType::String, true, "MESSAGE")],
        )
        .await;
        state
            .set_chamber("test-chamber".into(), make_chamber("test-chamber"))
            .await;
        Arc::new(make_service(state))
    }

    fn echo_request() -> CallToolRequest {
        CallToolRequest {
            name: "echo".to_string(),
            input_json: r#"{"message":"hello"}"#.to_string(),
        }
    }

    // AC-1: "When the controller accepts a chamber tool call, it shall return to
    // the caller the identifier it uses to track that call."
    #[tokio::test]
    async fn begin_tool_call_returns_the_tracking_call_id() {
        let svc = ready_service().await;

        let handle = svc
            .begin_tool_call(Request::new(echo_request()))
            .await
            .expect("begin_tool_call must not block on the result")
            .into_inner();
        assert!(
            !handle.call_id.is_empty(),
            "a tracking call_id must be returned"
        );

        // The returned id is *the* id tracking this call: the very next
        // GetToolCall assignment carries it. Materiality: if begin returns
        // String::new() the first assert reds; if it returns a fresh unrelated
        // uuid (not the enqueued call's id) this equality reds.
        let assignment = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            svc.get_tool_call(Request::new(GetToolCallRequest {
                job_id: "job-1".to_string(),
                tool_name: "echo".to_string(),
            })),
        )
        .await
        .expect("get_tool_call timed out")
        .unwrap()
        .into_inner();
        assert_eq!(
            assignment.call_id, handle.call_id,
            "the id returned to the caller must be the one tracking the enqueued call"
        );
    }

    // AC-4: "When the controller receives a cancel for a call identifier that is
    // unknown or already completed, it shall return successfully without error
    // and without corrupting its in-flight bookkeeping."
    #[tokio::test]
    async fn cancel_of_unknown_call_id_is_a_safe_no_op() {
        let svc = ready_service().await;

        // A genuine in-flight call, so we can prove the unknown cancel didn't
        // disturb its bookkeeping.
        let handle = svc
            .begin_tool_call(Request::new(echo_request()))
            .await
            .unwrap()
            .into_inner();

        let unknown = svc
            .cancel_tool_call(Request::new(airlock_proto::CancelToolCallRequest {
                call_id: "does-not-exist".to_string(),
            }))
            .await
            .expect("cancel of an unknown id must be Ok, never an error status")
            .into_inner();
        assert!(
            !unknown.cancelled,
            "an unknown/finished call reports cancelled=false"
        );

        // The real call is still cancellable — proving the unknown cancel did
        // not remove/corrupt the real entry. Materiality: a mutant that returns
        // NotFound/panics on the unknown id reds the unwrap above; a mutant that
        // removes a wrong entry reds this true.
        let real = svc
            .cancel_tool_call(Request::new(airlock_proto::CancelToolCallRequest {
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

    // AC-5: "When a chamber tool call is killed, the caller's parked wait shall
    // be unblocked with a terminal result rather than left awaiting
    // indefinitely."
    #[tokio::test]
    async fn await_tool_result_unblocks_on_killed_send_tool_result() {
        let svc = ready_service().await;

        let handle = svc
            .begin_tool_call(Request::new(echo_request()))
            .await
            .unwrap()
            .into_inner();
        let call_id = handle.call_id.clone();

        let svc_await = svc.clone();
        let await_call_id = call_id.clone();
        let await_handle = tokio::spawn(async move {
            svc_await
                .await_tool_result(Request::new(airlock_proto::AwaitToolResultRequest {
                    call_id: await_call_id,
                }))
                .await
        });

        tokio::task::yield_now().await;

        // The runtime reports a signal-terminated (killed) result.
        svc.send_tool_result(Request::new(SendToolResultRequest {
            call_id,
            output: "killed by cancel".to_string(),
            is_error: true,
            exit_code: -1,
        }))
        .await
        .unwrap();

        // Materiality: if the killed SendToolResult never fires the parked
        // awaiter's result channel, this await never returns and the timeout
        // reds.
        let resp = tokio::time::timeout(std::time::Duration::from_secs(2), await_handle)
            .await
            .expect("a killed result must unblock the parked awaiter")
            .unwrap()
            .unwrap()
            .into_inner();
        assert_eq!(resp.output, "killed by cancel");
        assert!(
            resp.is_error,
            "a killed call surfaces as a terminal error result"
        );
    }

    // AC-3 (delivery seam): the runtime-facing AwaitToolCancel long-poll blocks
    // until a cancel for that call fires, then returns — the channel by which a
    // CancelToolCall reaches the executing runtime.
    #[tokio::test]
    async fn await_tool_cancel_returns_when_cancel_fires() {
        let svc = ready_service().await;

        let handle = svc
            .begin_tool_call(Request::new(echo_request()))
            .await
            .unwrap()
            .into_inner();
        let call_id = handle.call_id.clone();

        let svc_poll = svc.clone();
        let poll_call_id = call_id.clone();
        let cancel_poll = tokio::spawn(async move {
            svc_poll
                .await_tool_cancel(Request::new(airlock_proto::AwaitToolCancelRequest {
                    call_id: poll_call_id,
                }))
                .await
        });

        tokio::task::yield_now().await;
        assert!(
            !cancel_poll.is_finished(),
            "AwaitToolCancel must block until a cancel fires"
        );

        svc.cancel_tool_call(Request::new(airlock_proto::CancelToolCallRequest {
            call_id,
        }))
        .await
        .unwrap();

        // Materiality: if CancelToolCall doesn't fire this call's cancel token,
        // the long-poll never returns and the timeout reds.
        tokio::time::timeout(std::time::Duration::from_secs(2), cancel_poll)
            .await
            .expect("AwaitToolCancel must return once the cancel fires")
            .unwrap()
            .unwrap();
    }
}
