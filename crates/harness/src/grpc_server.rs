//! Serves `HarnessControl` (harness-proto) for relay-controller.

use std::sync::Arc;

use harness_proto::harness_control_server::HarnessControl;
use proto_common::{
    AwaitToolResultRequest, CallToolRequest, CancelToolRequest, CancelToolResponse,
    CancelTurnRequest, CancelTurnResponse, ClearContextRequest, ClearContextResponse,
    CompactContextRequest, CompactContextResponse, DispatchToolResponse,
    GetConversationHistoryRequest, GetConversationHistoryResponse, HistoryEntry,
    ListSubAgentsRequest, ListSubAgentsResponse, Message, MessageSubAgentRequest,
    MessageSubAgentResponse, ReadSubAgentLogRequest, Sender, StopSubAgentRequest,
    StopSubAgentResponse, TakeBackMessageRequest, TakeBackMessageResponse, ToolInfo,
    ToolListUpdate, ToolResultFrame, WatchToolsRequest,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use toolset_proto::TurnRequest;

use crate::agent_state::AgentState;
use crate::clients::InferenceRpc;
use crate::conversation::{tool_turn_as_text, EntrySnapshot};
use crate::dispatch_hub::DispatchHub;
use crate::tool_router::{RelaySeam, ToolRouter};
use crate::turn::{consume_turn_stream, EmitState, NullSink, TurnAbort};

/// The fixed harness instruction that closes the one no-tools inference
/// call writing a compaction summary over the current window.
const COMPACT_INSTRUCTION: &str = "You are compacting this conversation. Write a summary of the \
conversation so far that keeps every fact, decision, open task and piece of context needed to \
carry on from here. Reply with the summary only.";

/// Builds a client for the agent's model. The message loop and each
/// compaction call take their own.
pub(crate) type InferenceFactory = Arc<dyn Fn() -> Box<dyn InferenceRpc> + Send + Sync>;

pub(crate) struct HarnessService {
    router: Arc<ToolRouter>,
    agent_state: Arc<AgentState>,
    /// The agent's relay, for the clear and compact entries.
    relay: RelaySeam,
    /// The agent's model, for the compaction call.
    inference: InferenceFactory,
    /// How long the compaction call's stream may go without an event.
    idle_gap: std::time::Duration,
}

impl HarnessService {
    pub(crate) fn new(
        router: Arc<ToolRouter>,
        agent_state: Arc<AgentState>,
        relay: RelaySeam,
        inference: InferenceFactory,
        idle_gap: std::time::Duration,
    ) -> Self {
        Self {
            router,
            agent_state,
            relay,
            inference,
            idle_gap,
        }
    }

    /// One no-tools inference call over `window` with the fixed compaction
    /// instruction. Returns the model's summary text.
    async fn summarize(&self, mut window: Vec<Message>) -> Result<String, Status> {
        // The call offers no tools, and a provider refuses tool calls and
        // tool results in a request that declares none, so they go as text.
        window = window.into_iter().map(tool_turn_as_text).collect();
        window.push(Message {
            role: "user".into(),
            content: proto_common::text_content(COMPACT_INSTRUCTION),
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        });
        let request = TurnRequest {
            system: None,
            tools: vec![],
            messages: window,
        };
        let mut model = (self.inference)();
        let mut source = model
            .turn(request)
            .await
            .map_err(|e| Status::unavailable(format!("compaction model call: {e}")))?;
        let result = consume_turn_stream(
            source.as_mut(),
            self.idle_gap,
            &mut NullSink,
            &mut EmitState::new(String::new()),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .map_err(|e| match e {
            TurnAbort::Ended(e) => Status::unavailable(format!("compaction model call: {e}")),
            TurnAbort::Cancelled => Status::cancelled("compaction model call cancelled"),
        })?;
        if result.stop_reason != proto_common::StopReason::EndTurn {
            return Err(Status::internal(format!(
                "compaction model did not finish the summary: {:?}",
                result.stop_reason
            )));
        }
        let summary = proto_common::content_text(&result.content);
        if summary.trim().is_empty() {
            return Err(Status::internal(
                "compaction model returned an empty summary",
            ));
        }
        Ok(summary)
    }

    /// Send the log's last entry, a clear or compact entry, through the
    /// message hook.
    async fn send_last_entry(&self) {
        let last = self.agent_state.log().read().await.snapshot(Some(1));
        let Some(entry) = last.entries.into_iter().next() else {
            return;
        };
        let sent = (self.relay)()
            .deliver_reply(entry.kind, entry.message.content)
            .await;
        if let Err(e) = sent {
            tracing::warn!(error = %e, "entry not sent");
        }
    }

    #[allow(clippy::result_large_err)]
    fn hub(&self) -> Result<&Arc<DispatchHub>, Status> {
        self.router
            .sub_agents()
            .ok_or_else(|| Status::failed_precondition("this agent has no sub-agents"))
    }
}

fn history_entry(e: EntrySnapshot) -> HistoryEntry {
    HistoryEntry {
        seq: e.seq,
        ts: e.ts,
        message: Some(e.message),
        sender: e.sender,
        kind: e.kind.into(),
    }
}

#[tonic::async_trait]
impl HarnessControl for HarnessService {
    type WatchToolsStream = ReceiverStream<Result<ToolListUpdate, Status>>;
    type AwaitToolResultStream = ReceiverStream<Result<ToolResultFrame, Status>>;

    async fn watch_tools(
        &self,
        _request: Request<WatchToolsRequest>,
    ) -> Result<Response<Self::WatchToolsStream>, Status> {
        let tools_proto = self.router.tool_definitions();
        let tools = tools_proto
            .into_iter()
            .map(|t| ToolInfo {
                toolset: String::new(),
                name: t.name,
                description: t.description,
                parameters_json: t.parameters_json,
            })
            .collect();
        let (tx, rx) = mpsc::channel(4);
        let _ = tx.send(Ok(ToolListUpdate { tools })).await;
        // Hold the channel open until the client disconnects: dropping tx
        // here would end the stream at once. The task ends when the
        // receiver is dropped.
        tokio::spawn(async move {
            tx.closed().await;
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn dispatch_tool(
        &self,
        request: Request<CallToolRequest>,
    ) -> Result<Response<DispatchToolResponse>, Status> {
        let req = request.into_inner();
        let call_id = self
            .router
            .dispatch_client_tool(&req.name, &req.input_json, &req.sender)
            .await
            .map_err(|e| Status::internal(format!("dispatch_tool: {e}")))?;
        Ok(Response::new(DispatchToolResponse { call_id }))
    }

    async fn await_tool_result(
        &self,
        request: Request<AwaitToolResultRequest>,
    ) -> Result<Response<Self::AwaitToolResultStream>, Status> {
        let req = request.into_inner();
        if req.call_id.is_empty() {
            return Err(Status::invalid_argument(
                "AwaitToolResultRequest.call_id required",
            ));
        }
        let stream = self.router.await_client_tool(&req.call_id).await?;
        Ok(Response::new(stream))
    }

    async fn cancel_tool(
        &self,
        request: Request<CancelToolRequest>,
    ) -> Result<Response<CancelToolResponse>, Status> {
        let req = request.into_inner();
        if req.call_id.is_empty() {
            return Err(Status::invalid_argument(
                "CancelToolRequest.call_id required",
            ));
        }
        let cancelled = self.router.cancel_client_tool(&req.call_id).await;
        Ok(Response::new(CancelToolResponse { cancelled }))
    }

    async fn cancel_turn(
        &self,
        _request: Request<CancelTurnRequest>,
    ) -> Result<Response<CancelTurnResponse>, Status> {
        let cancelled = self.agent_state.cancel();
        Ok(Response::new(CancelTurnResponse { cancelled }))
    }

    async fn take_back_message(
        &self,
        request: Request<TakeBackMessageRequest>,
    ) -> Result<Response<TakeBackMessageResponse>, Status> {
        // `sender` is set by the relay to the caller's bound subject, so a
        // person can only take back their own message.
        let req = request.into_inner();
        let content = self.agent_state.queue().take_back(&req.sender).await;
        Ok(Response::new(TakeBackMessageResponse {
            taken: content.is_some(),
            content: content.unwrap_or_default(),
        }))
    }

    async fn clear_context(
        &self,
        request: Request<ClearContextRequest>,
    ) -> Result<Response<ClearContextResponse>, Status> {
        let sender = Sender::person(request.into_inner().sender);
        let _slot = self.agent_state.acquire_turn_slot().await;
        self.agent_state
            .log()
            .write()
            .await
            .clear(sender)
            .await
            .map_err(|e| Status::internal(format!("clear conversation: {e}")))?;
        self.send_last_entry().await;
        Ok(Response::new(ClearContextResponse {}))
    }

    async fn compact_context(
        &self,
        request: Request<CompactContextRequest>,
    ) -> Result<Response<CompactContextResponse>, Status> {
        let sender = Sender::person(request.into_inner().sender);
        let _slot = self.agent_state.acquire_turn_slot().await;
        let log = self.agent_state.log();
        let window = log.read().await.history_for_provider();
        if window.is_empty() {
            return Err(Status::failed_precondition("nothing to compact"));
        }
        // The summary is written before the rotation, so a failed model call
        // leaves the window as it was.
        let summary = self.summarize(window).await?;
        log.write()
            .await
            .compact(&summary, sender)
            .await
            .map_err(|e| Status::internal(format!("compact conversation: {e}")))?;
        self.send_last_entry().await;
        Ok(Response::new(CompactContextResponse {}))
    }

    async fn get_conversation_history(
        &self,
        request: Request<GetConversationHistoryRequest>,
    ) -> Result<Response<GetConversationHistoryResponse>, Status> {
        let req = request.into_inner();
        let limit = req.limit.map(|n| n as usize);
        let snap = self.agent_state.log().read().await.snapshot(limit);
        let truncated = (snap.entries.len() as u64) < snap.total_seq;
        Ok(Response::new(GetConversationHistoryResponse {
            entries: snap.entries.into_iter().map(history_entry).collect(),
            total_seq: snap.total_seq,
            truncated,
        }))
    }

    async fn list_sub_agents(
        &self,
        _request: Request<ListSubAgentsRequest>,
    ) -> Result<Response<ListSubAgentsResponse>, Status> {
        let paths = self
            .hub()?
            .list()
            .await
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(ListSubAgentsResponse { paths }))
    }

    async fn read_sub_agent_log(
        &self,
        request: Request<ReadSubAgentLogRequest>,
    ) -> Result<Response<GetConversationHistoryResponse>, Status> {
        let path = request.into_inner().path;
        let entries = self
            .hub()?
            .read_log(&path)
            .await
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(GetConversationHistoryResponse {
            total_seq: entries.len() as u64,
            entries: entries.into_iter().map(history_entry).collect(),
            truncated: false,
        }))
    }

    async fn message_sub_agent(
        &self,
        request: Request<MessageSubAgentRequest>,
    ) -> Result<Response<MessageSubAgentResponse>, Status> {
        let req = request.into_inner();
        self.hub()?
            .message(&req.path, Sender::person(req.sender), &req.text)
            .await
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(MessageSubAgentResponse {}))
    }

    async fn stop_sub_agent(
        &self,
        request: Request<StopSubAgentRequest>,
    ) -> Result<Response<StopSubAgentResponse>, Status> {
        let path = request.into_inner().path;
        self.hub()?
            .stop(&path)
            .await
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(StopSubAgentResponse {}))
    }
}

// Tests for the client-facing dispatch / await / cancel handlers, driven
// directly with no live gRPC server.
#[cfg(test)]
mod dispatch_await_cancel_tests {
    use super::*;
    use proto_common::tool_result_frame::Frame;
    use proto_common::ToolOutcome;
    use std::time::Duration;
    use tokio_stream::StreamExt;

    use crate::dispatch::DispatchState;
    use crate::test_doubles::{
        claim_call, claimed, done_terminal, in_process_dispatch, seed, seed_job,
        spawn_claiming_pod, spawn_pod, stdout_frame, t, PodCapture,
    };
    use crate::tool_router::is_terminal_frame;

    /// Spawn a pod that feeds `pre` through, parks on `gate`, then feeds `post`
    /// and, when `err_after` is set, ends the stream on a frame error with no
    /// terminal — the abnormal-end path.
    fn spawn_gated_pod(
        dispatch: &Arc<DispatchState>,
        pre: Vec<ToolResultFrame>,
        gate: Arc<tokio::sync::Notify>,
        post: Vec<ToolResultFrame>,
        err_after: Option<String>,
    ) -> PodCapture {
        let captured: PodCapture = Arc::default();
        let pod_dispatch = dispatch.clone();
        let pod_captured = captured.clone();
        tokio::spawn(async move {
            let call = claim_call(&pod_dispatch, "Bash", "job-x", &pod_captured).await;
            let (tx, rx) = mpsc::channel::<Result<ToolResultFrame, Status>>(16);
            let feeder = tokio::spawn(async move {
                for f in pre {
                    let _ = tx.send(Ok(f)).await;
                }
                gate.notified().await;
                for f in post {
                    let _ = tx.send(Ok(f)).await;
                }
                if let Some(msg) = err_after {
                    let _ = tx.send(Err(Status::internal(msg))).await;
                }
            });
            let _ = pod_dispatch
                .forward_result_frames(call, ReceiverStream::new(rx))
                .await;
            let _ = feeder.await;
        });
        captured
    }

    async fn dispatch(svc: &HarnessService) -> String {
        svc.dispatch_tool(Request::new(CallToolRequest {
            name: "Bash".into(),
            input_json: "{}".into(),
            ..Default::default()
        }))
        .await
        .expect("dispatch returns Ok")
        .into_inner()
        .call_id
    }

    async fn subscribe(
        svc: &HarnessService,
        call_id: &str,
    ) -> <HarnessService as HarnessControl>::AwaitToolResultStream {
        svc.await_tool_result(Request::new(AwaitToolResultRequest {
            call_id: call_id.to_string(),
            ..Default::default()
        }))
        .await
        .expect("await returns a frame stream")
        .into_inner()
    }

    /// Read `stream` until its terminal frame or its end.
    async fn collect(
        stream: <HarnessService as HarnessControl>::AwaitToolResultStream,
    ) -> Vec<ToolResultFrame> {
        let mut stream = std::pin::pin!(stream);
        let mut frames = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
                Ok(Some(Ok(f))) => {
                    let done = is_terminal_frame(&f);
                    frames.push(f);
                    if done {
                        break;
                    }
                }
                Ok(Some(Err(e))) => panic!("await stream errored: {e}"),
                Ok(None) => break,
                Err(_) => panic!(
                    "await stream stalled before the terminal, collected {} frames",
                    frames.len()
                ),
            }
        }
        frames
    }

    // Dispatch surfaces the minted call_id to the client BEFORE the call
    // resolves. No pod claims the enqueued call, so it never terminates; the only
    // way dispatch can return is by NOT waiting on the result — it enqueues,
    // spawns the frame consumer, and returns the id.
    #[tokio::test]
    async fn dispatch_surfaces_the_minted_call_id_before_the_result() {
        let (svc, _root, _dispatcher) = service_and_root().await;
        let call_id = tokio::time::timeout(Duration::from_secs(3), dispatch(&svc))
            .await
            .expect("dispatch must return the call_id without awaiting the result");
        assert!(
            !call_id.is_empty(),
            "dispatch surfaces the minted call_id before the call resolves"
        );
    }

    // A CancelTool for an in-flight call fires the call's cancel token in the
    // in-process dispatcher, which closes the call's Run stream to the pod.
    // The claiming pod streams no terminal, so the call
    // is still in-flight when the cancel arrives.
    #[tokio::test]
    async fn cancel_of_an_in_flight_call_forwards_to_the_toolset() {
        let (svc, _root, dispatcher) = service_and_root().await;
        let call_id = dispatch(&svc).await;
        // A pod claims the call but streams nothing, so it stays in flight with
        // its cancel token still registered.
        let capture = spawn_claiming_pod(&dispatcher, "Bash", "job-x");
        let claimed_id = claimed(&capture).await;
        assert_eq!(claimed_id, call_id);

        let resp = svc
            .cancel_tool(Request::new(CancelToolRequest {
                call_id: call_id.clone(),
            }))
            .await
            .expect("cancel returns Ok")
            .into_inner();
        assert!(
            resp.cancelled,
            "canceling an in-flight call reports it canceled"
        );
        // The cancel fired the call's token exactly once: a second fire finds no
        // token, proving the harness forwarded the cancel into the dispatcher
        // rather than dropping it.
        assert!(
            !dispatcher.fire_call_cancel(&call_id).await,
            "the in-flight call's cancel token was fired exactly once"
        );
    }

    // A CancelTool naming a call_id with no in-flight call reports that no
    // call was canceled — and fires nothing in the dispatcher (there is nothing
    // to cancel). A real in-flight call's token is left intact, so a stray fire
    // is observable.
    #[tokio::test]
    async fn cancel_of_an_unknown_call_id_reports_none_canceled() {
        let (svc, _root, dispatcher) = service_and_root().await;
        // A real call kept in flight, so its token is live for the survives check.
        let call_id = dispatch(&svc).await;
        let capture = spawn_claiming_pod(&dispatcher, "Bash", "job-x");
        claimed(&capture).await;

        let resp = svc
            .cancel_tool(Request::new(CancelToolRequest {
                call_id: "never-dispatched".into(),
            }))
            .await
            .expect("cancel returns Ok")
            .into_inner();
        assert!(
            !resp.cancelled,
            "canceling a call_id with no in-flight call reports that no call was canceled"
        );
        assert!(
            dispatcher.fire_call_cancel(&call_id).await,
            "an unknown call_id fires nothing, leaving the real call's token intact"
        );
    }

    // The live in-flight follow path: while a call is GENUINELY still in
    // flight, a client that subscribes mid-stream receives the terminal frame
    // through the live fan-out — not just the frames already buffered at
    // subscribe time. The gated pod parks its consumer on the gate just
    // before the terminal, so at subscribe the call is still present in the
    // router's session map and its snapshot holds no terminal; the terminal is
    // released (via the gate) only AFTER the client has subscribed, so it can
    // reach the client only through the in-flight follow-loop, never the
    // persisted-fallback path the sibling await test lands on.
    #[tokio::test]
    async fn await_follows_a_live_call_to_its_terminal_via_the_fan_out() {
        use tokio::sync::Notify;

        let (svc, _root, dispatcher) = service_and_root().await;
        let gate = Arc::new(Notify::new());
        spawn_gated_pod(
            &dispatcher,
            vec![stdout_frame("pre-subscribe-chunk")],
            gate.clone(),
            vec![done_terminal()],
            None,
        );

        let call_id = dispatch(&svc).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Subscribe while the call is genuinely in flight. `await_client_tool`
        // snapshots the frames and subscribes to the fan-out synchronously before
        // returning, so releasing the gate now forces the terminal down the
        // live-follow loop, not the snapshot.
        let stream = subscribe(&svc, &call_id).await;
        gate.notify_one();
        let frames = collect(stream).await;

        assert!(
            frames.iter().any(
                |f| matches!(f.frame.as_ref(), Some(Frame::Stdout(s)) if s == "pre-subscribe-chunk")
            ),
            "the client receives the in-flight call's frames as a live stream, got {} frames",
            frames.len()
        );
        let last = frames
            .last()
            .expect("the live-follow terminal must reach the client");
        match last.frame.as_ref() {
            Some(Frame::Complete(c)) => assert_eq!(
                c.outcome(),
                ToolOutcome::Done,
                "the DONE terminal emitted after subscribe reaches the client through the live in-flight follow-loop, got {:?}",
                c.outcome()
            ),
            other => panic!("the await stream must end on the scripted DONE terminal, got {other:?}"),
        }
    }

    // A client-driven call whose pod frame stream ends abnormally — one or two
    // frames, then a frame-stream error with no terminal ToolComplete — must
    // still terminate the client's await stream in a terminal outcome. When the
    // pod's stream errors, `forward_result_frames` drops the call's result guard
    // without marking it complete, so the guard injects a synthetic FAILED
    // terminal into the call's drain; the session consumer publishes it and an
    // awaiter following the live fan-out sees it. Every client-driven call must
    // end in one of the three outcomes; a synthetic FAILED terminal is owed when
    // the stream dies mid-call.
    //
    // The gated abnormal stream keeps the call genuinely in flight at subscribe,
    // so the terminalless close is reached via the in-flight follow branch, not
    // the persisted-fallback path.
    #[tokio::test]
    async fn await_of_an_abnormally_ended_live_call_ends_on_a_failed_terminal() {
        use tokio::sync::Notify;

        let (svc, _root, dispatcher) = service_and_root().await;
        let gate = Arc::new(Notify::new());
        spawn_gated_pod(
            &dispatcher,
            vec![stdout_frame("partial-then-death")],
            gate.clone(),
            vec![],
            Some("toolset frame stream broke".into()),
        );

        let call_id = dispatch(&svc).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let stream = subscribe(&svc, &call_id).await;

        // Release the gate: the pod's stream errors, and the dropped guard's
        // FAILED terminal reaches the client.
        gate.notify_one();
        let frames = collect(stream).await;

        let last = frames
            .last()
            .expect("an abnormally ended call must still yield a terminal frame to the client");
        match last.frame.as_ref() {
            Some(Frame::Complete(c)) => {
                assert_eq!(
                    c.outcome(),
                    ToolOutcome::Failed,
                    "an abnormal stream end must terminate the client's await on a synthetic FAILED terminal, got {:?}",
                    c.outcome()
                );
                assert_eq!(
                    c.exit_code, -1,
                    "the synthetic FAILED terminal carries the sentinel exit_code -1, got {}",
                    c.exit_code
                );
            }
            other => panic!("the await stream must end on a terminal ToolComplete, got {other:?}"),
        }
    }

    /// A service whose agent state is rooted at a fresh tempdir; frames persist at
    /// `<root>/execution.json`.
    async fn service_and_root() -> (HarnessService, std::path::PathBuf, Arc<DispatchState>) {
        let root = tempfile::TempDir::new().unwrap().keep();
        let agent_state = Arc::new(AgentState::open(root.clone(), 10).await.unwrap());
        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let router: Arc<ToolRouter> = Arc::new(seed(
            ToolRouter::new(
                crate::test_doubles::test_instructions(),
                crate::test_doubles::TEST_AG.to_string(),
                None,
                agent_state.clone(),
            )
            .with_dispatch(dispatch.clone()),
            vec![t("Bash")],
        ));
        let svc = HarnessService::new(
            router,
            agent_state,
            crate::tool_router::relay_seam(crate::test_doubles::ScriptedRelay::default()),
            Arc::new(|| {
                Box::new(crate::test_doubles::ScriptedTurns::new(vec![])) as Box<dyn InferenceRpc>
            }),
            std::time::Duration::from_secs(45),
        );
        (svc, root, dispatch)
    }

    // An app-dispatched tool appends its frames to the agent's one
    // execution.json; re-subscribe replays them filtered by call_id.
    #[tokio::test]
    async fn app_dispatch_persists_frames_to_the_agents_execution_json() {
        let (svc, root, dispatcher) = service_and_root().await;
        spawn_pod(
            &dispatcher,
            "Bash",
            "job-x",
            vec![stdout_frame("app-run-marker"), done_terminal()],
        );

        let call_id = dispatch(&svc).await;
        assert!(!call_id.is_empty(), "dispatch surfaces the minted call_id");

        // Let the consumer drain both frames and retire the session so the await is
        // served from the persisted record on disk, not the live fan-out.
        tokio::time::sleep(Duration::from_millis(80)).await;

        let exec_json = root.join("execution.json");
        assert!(
            exec_json.is_file(),
            "app-run frames must persist to the agent's execution.json at {}",
            exec_json.display()
        );
        let text = std::fs::read_to_string(&exec_json).unwrap();
        assert!(
            text.contains("app-run-marker"),
            "the call's frames are appended to the agent's execution.json, got {text:?}"
        );

        let frames = collect(subscribe(&svc, &call_id).await).await;
        assert!(
            frames.iter().any(
                |f| matches!(f.frame.as_ref(), Some(Frame::Stdout(s)) if s == "app-run-marker")
            ),
            "re-subscribe replays the persisted stdout frame filtered by call_id, got {} frames",
            frames.len()
        );
        assert!(
            frames.last().map(is_terminal_frame).unwrap_or(false),
            "the replayed stream ends on the terminal frame"
        );
    }
}

// The agent's one conversation through the relay-facing surface.
#[cfg(test)]
mod context_tests {
    use super::*;
    use crate::test_doubles::{
        complete_turn, record, ArchiveStep, ArchiveStub, ScriptedRelay, ScriptedTurns, ALICE, BOB,
        TEST_AG,
    };
    use proto_common::{content_text, EntryKind};
    use std::fs;

    /// A service over a tempdir whose `conversation.log` holds `log`, answering
    /// model calls from `model`, and the relay it sends entries to.
    async fn service_with(
        log: &str,
        model: ScriptedTurns,
    ) -> (
        HarnessService,
        tempfile::TempDir,
        Arc<AgentState>,
        ScriptedRelay,
    ) {
        let relay = ScriptedRelay::default();
        let root = tempfile::TempDir::new().unwrap();
        fs::write(root.path().join("conversation.log"), log).unwrap();
        let agent_state = Arc::new(
            AgentState::open(root.path().to_path_buf(), 10)
                .await
                .unwrap(),
        );
        let svc = HarnessService::new(
            crate::test_doubles::test_router(agent_state.clone()),
            agent_state.clone(),
            crate::tool_router::relay_seam(relay.clone()),
            Arc::new(move || Box::new(model.clone()) as Box<dyn InferenceRpc>),
            std::time::Duration::from_secs(45),
        );
        (svc, root, agent_state, relay)
    }

    async fn history(svc: &HarnessService) -> Vec<(EntryKind, Option<Sender>, String)> {
        svc.get_conversation_history(Request::new(GetConversationHistoryRequest::default()))
            .await
            .expect("history returns Ok")
            .into_inner()
            .entries
            .into_iter()
            .map(|e| {
                let text = content_text(&e.message.clone().unwrap_or_default().content);
                (e.kind(), e.sender, text)
            })
            .collect()
    }

    #[tokio::test]
    async fn history_limit_clips_to_the_tail_and_only_then_reports_truncated() {
        let current: String = (0..3).map(|n| record("user", &format!("m{n}"))).collect();
        let (svc, _root, _, _) = service_with(&current, ScriptedTurns::new(vec![])).await;
        for (limit, want_truncated, want_seqs) in
            [(Some(2), true, vec![2, 3]), (None, false, vec![1, 2, 3])]
        {
            let resp = svc
                .get_conversation_history(Request::new(GetConversationHistoryRequest {
                    limit,
                    ..Default::default()
                }))
                .await
                .expect("history returns Ok")
                .into_inner();
            assert_eq!(
                resp.truncated, want_truncated,
                "limit {limit:?}: only a limit below the entry count reports truncated"
            );
            let seqs: Vec<u64> = resp.entries.iter().map(|e| e.seq).collect();
            assert_eq!(
                seqs, want_seqs,
                "limit {limit:?}: the returned tail is exactly the requested limit, or all"
            );
        }
    }

    // A clear or compact sent while a turn holds the turn slot waits for the
    // turn to end: no error, no summary call and no change to the log until
    // then, and once the turn ends the clear leaves one clear entry from the
    // person who asked.
    #[tokio::test]
    async fn clear_and_compact_wait_for_a_running_turn_to_end() {
        let current = record("user", "mid-turn window");
        let model = ScriptedTurns::new(vec![]);
        let recorder = model.clone();
        let (svc, root, agent_state, _) = service_with(&current, model).await;

        let log_path = root.path().join("conversation.log");
        let still_waiting = std::time::Duration::from_millis(200);
        let turn = agent_state.acquire_turn_slot().await;

        let compact = svc.compact_context(Request::new(CompactContextRequest {
            agent: TEST_AG.into(),
            sender: ALICE.into(),
        }));
        assert!(
            tokio::time::timeout(still_waiting, compact).await.is_err(),
            "compact waits while a turn runs"
        );
        assert!(recorder.requests().is_empty(), "no summary call mid-turn");

        let clear = svc.clear_context(Request::new(ClearContextRequest {
            agent: TEST_AG.into(),
            sender: ALICE.into(),
        }));
        tokio::pin!(clear);
        assert!(
            tokio::time::timeout(still_waiting, &mut clear)
                .await
                .is_err(),
            "clear waits while a turn runs"
        );
        assert_eq!(
            fs::read_to_string(&log_path).unwrap(),
            current,
            "the log is unchanged mid-turn"
        );
        assert!(!root.path().join("conversation.log.1.gz").exists());

        drop(turn);
        clear.await.expect("clear runs once the turn ends");
        assert_eq!(
            history(&svc).await,
            [(EntryKind::Clear, Some(Sender::person(ALICE)), String::new())],
            "after the turn the log holds one clear entry from the person, with no content"
        );
    }

    // A compact and then a clear each leave one entry of their kind in the
    // history read after it, from the person who asked, and each sends that
    // entry through the message hook.
    #[tokio::test]
    async fn clear_and_compact_each_leave_and_send_one_entry_of_their_kind_from_the_person_who_asked(
    ) {
        let model = ScriptedTurns::new(vec![complete_turn(
            proto_common::StopReason::EndTurn,
            "the door is blue",
            &[],
        )]);
        let (svc, root, agent_state, relay) =
            service_with(&record("user", "remember the blue door"), model).await;
        *agent_state.log().write().await = crate::conversation::ConversationLog::rebuild(Arc::new(
            ArchiveStub::new(root.path(), ArchiveStep::RenameOk),
        ))
        .await
        .unwrap();

        svc.compact_context(Request::new(CompactContextRequest {
            agent: TEST_AG.into(),
            sender: ALICE.into(),
        }))
        .await
        .expect("the compact succeeds");
        let compacted = history(&svc).await;
        let summary = compacted
            .first()
            .map(|(_, _, text)| text.clone())
            .unwrap_or_default();
        assert!(
            summary.contains("the door is blue"),
            "the compact entry holds the summary, got {compacted:?}"
        );
        assert_eq!(
            compacted,
            [(
                EntryKind::Compact,
                Some(Sender::person(ALICE)),
                summary.clone()
            )],
            "the compact leaves one compact entry from the person who asked"
        );

        svc.clear_context(Request::new(ClearContextRequest {
            agent: TEST_AG.into(),
            sender: BOB.into(),
        }))
        .await
        .expect("the clear succeeds");
        let cleared = [(EntryKind::Clear, Some(Sender::person(BOB)), String::new())];
        assert_eq!(
            history(&svc).await,
            cleared,
            "the clear leaves one clear entry from the person who asked"
        );

        let refused = svc
            .compact_context(Request::new(CompactContextRequest {
                agent: TEST_AG.into(),
                sender: ALICE.into(),
            }))
            .await
            .expect_err("a compact after the clear is refused");
        assert_eq!(refused.message(), "nothing to compact");
        assert_eq!(
            history(&svc).await,
            cleared,
            "a compact after the clear leaves the one clear entry"
        );

        assert_eq!(
            relay.entries(),
            [
                (EntryKind::Compact, summary),
                (EntryKind::Clear, String::new()),
            ],
            "each entry goes through the message hook once, as written"
        );
    }

    // The compaction summary is one inference call over the current window
    // that offers the model no tools. It carries the window's text, its tool
    // calls and results included, as text with no tool roles, and it closes
    // on a user message holding the compact instruction.
    #[tokio::test]
    async fn summary_request_is_one_no_tools_call_closing_with_the_compact_instruction() {
        let current = format!(
            "{}{}{}{}",
            record("user", "remember the blue door"),
            "{\"ts\":\"t\",\"role\":\"assistant\",\"tool_calls\":[{\"id\":\"tc1\",\"name\":\"Shell\",\"input_json\":\"{\\\"cmd\\\":\\\"ls /srv\\\"}\"}]}\n",
            "{\"ts\":\"t\",\"role\":\"tool\",\"content\":[{\"block\":{\"Text\":{\"text\":\"door.txt\"}}}],\"tool_call_id\":\"tc1\"}\n",
            record("assistant", "noted")
        );
        let model = ScriptedTurns::new(vec![complete_turn(
            proto_common::StopReason::EndTurn,
            "the door is blue",
            &[],
        )]);
        let recorder = model.clone();
        let (svc, _root, agent_state, _) = service_with(&current, model).await;
        let window = agent_state.log().read().await.history_for_provider();

        let summary = svc
            .summarize(window)
            .await
            .expect("the model writes the summary");
        assert_eq!(summary, "the door is blue");

        let requests = recorder.requests();
        assert_eq!(requests.len(), 1, "compaction makes exactly one model call");
        let sent: Vec<String> = requests[0]
            .messages
            .iter()
            .map(|m| content_text(&m.content))
            .collect();
        for want in ["remember the blue door", "ls /srv", "door.txt", "noted"] {
            assert!(
                sent.iter().any(|t| t.contains(want)),
                "the summary request carries the current window's {want:?}, got {sent:?}"
            );
        }
        assert!(
            requests[0].tools.is_empty(),
            "the summary request offers the model no tools"
        );
        assert!(
            requests[0]
                .messages
                .iter()
                .all(|m| m.tool_calls.is_empty() && m.tool_call_id.is_none() && m.role != "tool"),
            "with no tools offered, the window's tool calls and results go as text"
        );
        let last = requests[0]
            .messages
            .last()
            .expect("the summary request has messages");
        assert_eq!(
            last.role, "user",
            "the summary request closes on a user message"
        );
        assert_eq!(
            content_text(&last.content),
            COMPACT_INSTRUCTION,
            "the closing user message is the compact instruction, got {sent:?}"
        );
    }

    #[tokio::test]
    async fn a_cut_off_summary_is_refused() {
        let model = ScriptedTurns::new(vec![complete_turn(
            proto_common::StopReason::MaxTokens,
            "the door is",
            &[],
        )]);
        let (svc, _root, agent_state, _) =
            service_with(&record("user", "remember the blue door"), model).await;
        let window = agent_state.log().read().await.history_for_provider();

        assert!(
            svc.summarize(window).await.is_err(),
            "a summary cut off at the length limit is refused"
        );
    }
}

// The four sub-agent commands a person sends, served through the hub.
#[cfg(test)]
mod sub_agent_tests {
    use super::*;
    use proto_common::content_text;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    use crate::conversation::{AssistantAttribution, ConversationLog, LocalFsStore};
    use crate::dispatch_hub::test_support::*;
    use crate::dispatch_link::ParentLink;
    use crate::queue::Inbox;
    use crate::test_doubles::{test_instructions, text_msg, ScriptedRelay, ALICE, TEST_AG};

    const PATH: &str = "sub/0a1b2c3d";
    const OTHER: &str = "sub/12345678";

    /// A hub of one fake sub-agent attached at `PATH`, with `listed` ready,
    /// and the service and router over the hub's own agent state.
    async fn one_sub_agent(
        listed: Vec<String>,
    ) -> (TestHub, HarnessService, Arc<ToolRouter>, ParentLink, Inbox) {
        let inbox = Inbox::default();
        let link = sub_agent_link_with(inbox.clone());
        let uris = endpoints(vec![(PATH, serve(link.clone()))], silent_endpoint());
        let h = test_hub(uris, ScriptedRelay::default(), listed);
        h.hub.attach(PATH.into());
        attached(&h, PATH).await;
        let router = Arc::new(
            ToolRouter::new(
                test_instructions(),
                TEST_AG.to_string(),
                None,
                h.agent_state.clone(),
            )
            .with_sub_agents(h.hub.clone()),
        );
        let svc = HarnessService::new(
            router.clone(),
            h.agent_state.clone(),
            crate::tool_router::relay_seam(ScriptedRelay::default()),
            Arc::new(|| {
                Box::new(crate::test_doubles::ScriptedTurns::new(vec![])) as Box<dyn InferenceRpc>
            }),
            Duration::from_secs(45),
        );
        (h, svc, router, link, inbox)
    }

    #[tokio::test]
    async fn list_sub_agents_returns_the_agents_ready_paths() {
        let listed = vec![PATH.to_string(), OTHER.to_string()];
        let (_h, svc, _, _, _) = one_sub_agent(listed.clone()).await;
        let resp = svc
            .list_sub_agents(Request::new(ListSubAgentsRequest {
                agent: TEST_AG.into(),
            }))
            .await
            .expect("list returns Ok")
            .into_inner();
        assert_eq!(resp.paths, listed);
    }

    #[tokio::test]
    async fn read_sub_agent_log_returns_each_entry_with_its_sender() {
        let (h, svc, _, _, _) = one_sub_agent(vec![]).await;
        let folder = h.root.join(PATH);
        std::fs::create_dir_all(&folder).unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(folder, 0)));
        log.append_user(text_msg("user", "the mandate"), Sender::parent_agent(AGENT))
            .await
            .unwrap();
        log.append_user(text_msg("user", "also tulip"), Sender::person(ALICE))
            .await
            .unwrap();
        log.append_assistant(
            text_msg("assistant", "the answer"),
            AssistantAttribution::default(),
        )
        .await
        .unwrap();

        let resp = svc
            .read_sub_agent_log(Request::new(ReadSubAgentLogRequest {
                agent: TEST_AG.into(),
                path: PATH.into(),
            }))
            .await
            .expect("read log returns Ok")
            .into_inner();
        let got: Vec<(String, String, Option<Sender>)> = resp
            .entries
            .into_iter()
            .map(|e| {
                let m = e.message.unwrap_or_default();
                (m.role, content_text(&m.content), e.sender)
            })
            .collect();
        assert_eq!(
            got,
            [
                (
                    "user".into(),
                    "the mandate".into(),
                    Some(Sender::parent_agent(AGENT))
                ),
                (
                    "user".into(),
                    "also tulip".into(),
                    Some(Sender::person(ALICE))
                ),
                ("assistant".into(), "the answer".into(), None),
            ]
        );
    }

    #[tokio::test]
    async fn message_sub_agent_lands_in_the_sub_agent_from_the_person() {
        let (_h, svc, _, _, inbox) = one_sub_agent(vec![]).await;
        svc.message_sub_agent(Request::new(MessageSubAgentRequest {
            agent: TEST_AG.into(),
            path: PATH.into(),
            text: "also tulip".into(),
            sender: ALICE.into(),
        }))
        .await
        .expect("message returns Ok");
        assert_eq!(
            landed_from(&inbox).await,
            [(Sender::person(ALICE), "also tulip".to_string())]
        );
    }

    #[tokio::test]
    async fn stop_sub_agent_stops_the_sub_agent() {
        let (_h, svc, _, link, _) = one_sub_agent(vec![]).await;
        svc.stop_sub_agent(Request::new(StopSubAgentRequest {
            agent: TEST_AG.into(),
            path: PATH.into(),
        }))
        .await
        .expect("stop returns Ok");
        tokio::time::timeout(Duration::from_secs(5), link.stopped())
            .await
            .expect("the sub-agent stops");
    }

    #[tokio::test]
    async fn a_persons_message_sub_agent_call_lands_from_the_person_and_the_agents_from_the_parent_agent(
    ) {
        let (_h, svc, router, _, inbox) = one_sub_agent(vec![]).await;
        let input = |text: &str| serde_json::json!({ "path": PATH, "message": text }).to_string();

        svc.dispatch_tool(Request::new(CallToolRequest {
            name: "MessageSubAgent".into(),
            input_json: input("from alice"),
            sender: ALICE.into(),
            ..Default::default()
        }))
        .await
        .expect("dispatch returns Ok");
        assert_eq!(
            landed_from(&inbox).await,
            [(Sender::person(ALICE), "from alice".to_string())],
            "the person's call lands from the person"
        );

        let resp = router
            .call_tool(
                "MessageSubAgent",
                &input("from the agent"),
                false,
                "tc",
                &CancellationToken::new(),
            )
            .await
            .expect("a tool result");
        assert!(!resp.is_error, "the agent's call reaches the hub");
        assert_eq!(
            landed_from(&inbox).await,
            [(Sender::parent_agent(AGENT), "from the agent".to_string())],
            "the agent's call lands from the parent agent"
        );
    }
}
