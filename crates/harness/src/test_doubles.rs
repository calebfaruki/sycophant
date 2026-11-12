//! Shared `#[cfg(test)]` test doubles reachable across the crate's unit-test
//! modules.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use proto_common::tool_result_frame::Frame;
use proto_common::{
    ContentBlock, StreamItem, ToolComplete, ToolOutcome, ToolResultFrame, TurnStateEvent,
};
use shared::scheduling::SchedulingConfig;
use shared::toolset::{ToolDecl, ToolsetConfig, ToolsetEntry};
use tokio_util::sync::CancellationToken;
use toolset_proto::{turn_event, TurnEvent, TurnRequest};

use crate::agent_state::AgentState;
use crate::clients::{InferenceRpc, RelayRpc, ServerRequestOutcome, TurnSource};
use crate::conversation::{ConversationLog, LocalFsStore};
use crate::dispatch::{ActiveJob, DispatchState};
use crate::instructions::Instructions;
use crate::tool_router::ToolRouter;

/// The agent name used by test routers/instructions.
pub(crate) const TEST_AG: &str = "ag";

/// An empty-agent instructions over a throwaway temp dir. The dir is leaked so
/// the returned `Arc<Instructions>` can outlive this call; tests that need populated
/// content build their own `Instructions` directly.
pub(crate) fn test_instructions() -> Arc<Instructions> {
    let root = tempfile::TempDir::new().unwrap().keep();
    std::fs::create_dir_all(root.join(TEST_AG)).unwrap();
    Arc::new(Instructions::new(root))
}

/// A toolset tool with no args.
pub(crate) fn t(name: &str) -> ToolDecl {
    ToolDecl {
        name: name.into(),
        description: format!("desc:{name}"),
        args: Vec::new(),
    }
}

/// Scope `router` to one toolset "ts" holding `tools`.
pub(crate) fn seed(router: ToolRouter, tools: Vec<ToolDecl>) -> ToolRouter {
    let router =
        router.with_toolset_scope(vec!["ts".to_string()], std::collections::HashMap::new());
    let entry = ToolsetEntry {
        tools,
        ..Default::default()
    };
    let warnings =
        router.apply_toolsets(&ToolsetConfig::from_map(std::collections::HashMap::from([
            ("ts".to_string(), entry),
        ])));
    assert!(warnings.is_empty(), "{warnings:?}");
    router
}

/// The agent state opened over `root` (a compact keeps 10 archives). Opens
/// on its own thread and runtime, so sync and async tests can both call it.
pub(crate) fn open_agent_state(root: std::path::PathBuf) -> AgentState {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(AgentState::open(root, 10))
            .unwrap()
    })
    .join()
    .unwrap()
}

/// The agent state over a fresh empty temp dir. The dir is leaked, like
/// [`test_instructions`]'s.
pub(crate) fn empty_agent_state() -> AgentState {
    open_agent_state(tempfile::TempDir::new().unwrap().keep())
}

/// A conversation log over a fresh temp dir. The dir is leaked, like
/// [`test_instructions`]'s.
pub(crate) fn throwaway_log() -> ConversationLog {
    ConversationLog::new(Arc::new(LocalFsStore::new(
        tempfile::TempDir::new().unwrap().keep(),
        10,
    )))
}

pub(crate) fn write_md(root: &std::path::Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
}

pub(crate) async fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting until {what}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// A router over [`test_instructions`] and `agent_state`, with no relay.
pub(crate) fn test_router(agent_state: Arc<AgentState>) -> Arc<ToolRouter> {
    Arc::new(ToolRouter::new(
        test_instructions(),
        TEST_AG.to_string(),
        None,
        agent_state,
    ))
}

pub(crate) fn record(role: &str, text: &str) -> String {
    format!(
        "{{\"ts\":\"t\",\"role\":\"{role}\",\"content\":[{{\"block\":{{\"Text\":{{\"text\":\"{text}\"}}}}}}]}}\n"
    )
}

/// A shared, ordered log of what happened in a test, so a test can order
/// relay requests against pod claims and model turns.
pub(crate) type EventLog = Arc<Mutex<Vec<String>>>;

pub(crate) fn event_log() -> EventLog {
    Arc::new(Mutex::new(Vec::new()))
}

/// How the scripted relay answers one `send_server_request_and_await`.
pub(crate) enum RelayAnswer {
    Outcome(ServerRequestOutcome),
    Err(String),
    /// Never answers; only the caller dropping the future ends the wait.
    Never,
    /// Fires the token, then answers `{"approved":true}` in the same poll.
    ApproveAndCancel(CancellationToken),
}

impl RelayAnswer {
    pub(crate) fn approve() -> Self {
        Self::Outcome(ServerRequestOutcome::Result(r#"{"approved":true}"#.into()))
    }
    pub(crate) fn deny() -> Self {
        Self::Outcome(ServerRequestOutcome::Result(r#"{"approved":false}"#.into()))
    }
}

/// One `send_server_request_and_await` the relay received.
#[derive(Clone, Debug)]
pub(crate) struct RelayRequest {
    pub(crate) method: String,
    pub(crate) params_json: String,
    pub(crate) timeout_seconds: u32,
}

#[derive(Default)]
struct ScriptedRelayState {
    script: VecDeque<RelayAnswer>,
    requests: Vec<RelayRequest>,
}

/// A relay whose clones share one record and one script. Each
/// `send_server_request_and_await` is recorded, logged as
/// `request <method>` to the event log when one is set, and answered by the
/// next script entry. An empty script answers `Result("{}")`.
#[derive(Clone, Default)]
pub(crate) struct ScriptedRelay {
    state: Arc<Mutex<ScriptedRelayState>>,
    events: Option<EventLog>,
}

impl ScriptedRelay {
    pub(crate) fn new(script: Vec<RelayAnswer>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ScriptedRelayState {
                script: script.into(),
                ..Default::default()
            })),
            events: None,
        }
    }

    pub(crate) fn with_events(mut self, events: EventLog) -> Self {
        self.events = Some(events);
        self
    }

    /// The requests received for `method`, in order.
    pub(crate) fn requests_for(&self, method: &str) -> Vec<RelayRequest> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.method == method)
            .cloned()
            .collect()
    }
}

#[async_trait]
impl RelayRpc for ScriptedRelay {
    async fn send_server_notification(
        &mut self,
        _method: &str,
        _params_json: &str,
    ) -> Result<bool, String> {
        Ok(true)
    }
    async fn send_server_request_and_await(
        &mut self,
        _request_id: &str,
        method: &str,
        params_json: &str,
        timeout_seconds: u32,
    ) -> Result<ServerRequestOutcome, String> {
        let answer = {
            let mut state = self.state.lock().unwrap();
            state.requests.push(RelayRequest {
                method: method.to_string(),
                params_json: params_json.to_string(),
                timeout_seconds,
            });
            state.script.pop_front()
        };
        if let Some(events) = &self.events {
            events.lock().unwrap().push(format!("request {method}"));
        }
        match answer {
            None => Ok(ServerRequestOutcome::Result("{}".into())),
            Some(RelayAnswer::Outcome(outcome)) => Ok(outcome),
            Some(RelayAnswer::Err(e)) => Err(e),
            Some(RelayAnswer::Never) => std::future::pending().await,
            Some(RelayAnswer::ApproveAndCancel(token)) => {
                token.cancel();
                Ok(ServerRequestOutcome::Result(r#"{"approved":true}"#.into()))
            }
        }
    }
    async fn deliver_stream_item(&mut self, _item: StreamItem) -> Result<bool, String> {
        Ok(true)
    }
    async fn deliver_reply(&mut self, _content: Vec<ContentBlock>) -> Result<bool, String> {
        Ok(true)
    }
    async fn deliver_turn_state(&mut self, _turn_state: TurnStateEvent) -> Result<bool, String> {
        Ok(true)
    }
}

/// A model seam whose clones share one script of turns. Each `turn` call
/// replays the next scripted event list. An exhausted script fails the turn.
/// Every request received is recorded, in order, across clones.
#[derive(Clone, Default)]
pub(crate) struct ScriptedTurns {
    turns: Arc<Mutex<VecDeque<Vec<TurnEvent>>>>,
    requests: Arc<Mutex<Vec<TurnRequest>>>,
}

impl ScriptedTurns {
    pub(crate) fn new(turns: Vec<Vec<TurnEvent>>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
            requests: Arc::default(),
        }
    }

    pub(crate) fn requests(&self) -> Vec<TurnRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl InferenceRpc for ScriptedTurns {
    async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
        self.requests.lock().unwrap().push(request);
        let events = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "ScriptedTurns: no more scripted turns".to_string())?;
        Ok(Box::new(ScriptedSource {
            events: events.into(),
        }))
    }
}

/// A sink that records every streamed item.
pub(crate) struct CapturingSink(pub(crate) Vec<StreamItem>);

#[async_trait]
impl crate::turn::StreamSink for CapturingSink {
    async fn emit(&mut self, item: StreamItem) {
        self.0.push(item);
    }
}

pub(crate) struct ScriptedSource {
    pub(crate) events: VecDeque<TurnEvent>,
}

#[async_trait]
impl TurnSource for ScriptedSource {
    async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
        self.events.pop_front().map(Ok)
    }
}

/// A turn source whose `next_event` never resolves: a model that connected
/// and then went silent.
pub(crate) struct ParkedSource;

#[async_trait]
impl TurnSource for ParkedSource {
    async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
        std::future::pending().await
    }
}

/// A model turn that ends with `stop` after proposing `calls`, each given as
/// `(id, name, input_json)`.
pub(crate) fn complete_turn(
    stop: proto_common::StopReason,
    text: &str,
    calls: &[(&str, &str, &str)],
) -> Vec<TurnEvent> {
    let content = if text.is_empty() {
        vec![]
    } else {
        vec![proto_common::text_block(text.to_string())]
    };
    vec![TurnEvent {
        event: Some(turn_event::Event::Complete(toolset_proto::TurnComplete {
            stop_reason: stop as i32,
            content,
            tool_calls: calls
                .iter()
                .map(|(id, name, input)| proto_common::ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    input_json: input.to_string(),
                })
                .collect(),
        })),
    }]
}

pub(crate) fn content_delta(text: &str) -> TurnEvent {
    TurnEvent {
        event: Some(turn_event::Event::ContentDelta(
            toolset_proto::ContentDelta { text: text.into() },
        )),
    }
}

pub(crate) fn text_delta_of(item: &StreamItem) -> Option<&str> {
    match &item.phase {
        Some(proto_common::stream_item::Phase::Delta(proto_common::ItemDelta {
            kind: Some(proto_common::item_delta::Kind::TextDelta(t)),
        })) => Some(t),
        _ => None,
    }
}

// ---- In-process dispatch rig ----
//
// The `Source::Toolset` arm runs tool calls through the in-process
// `DispatchState`. These helpers stand that dispatcher up against a simulated
// tool-job pod: the pod helpers claim the enqueued call and feed scripted
// frames through the real `forward_result_frames` seam.

/// A dispatcher with no kube client over the toolsets `names`, each a warm
/// keepalive entry.
pub(crate) fn in_process_dispatch(names: &[&str]) -> Arc<DispatchState> {
    let entry = ToolsetEntry {
        image: Some("git:local".to_string()),
        keepalive: true,
        ..Default::default()
    };
    let toolsets = names
        .iter()
        .map(|name| (name.to_string(), entry.clone()))
        .collect();
    DispatchState::new(
        None,
        "ns".to_string(),
        "capability-ag".to_string(),
        TEST_AG.to_string(),
        &crate::config::Mode::Agent {
            relay_gateway_addr: String::new(),
            harness_image: String::new(),
        },
        SchedulingConfig::default(),
        Arc::new(tokio::sync::RwLock::new(ToolsetConfig::from_map(toolsets))),
    )
}

/// Seed the warm keepalive Job `begin_call` attaches `tool`'s calls to.
pub(crate) async fn seed_job(dispatch: &Arc<DispatchState>, tool: &str, job_id: &str) {
    dispatch
        .set_active_job(ActiveJob {
            job_name: format!("tool-{tool}-{job_id}"),
            job_id: job_id.to_string(),
            tool_name: tool.to_string(),
            keepalive_seconds: 600,
            last_activity: std::time::Instant::now(),
        })
        .await;
}

/// The call a pod claimed: its minted call_id.
pub(crate) type PodCapture = Arc<Mutex<Option<String>>>;

/// Poll `dequeue_call` for `(tool, job_id)`, record the claimed call's id in
/// `captured` and return it.
pub(crate) async fn claim_call(
    dispatch: &Arc<DispatchState>,
    tool: &str,
    job_id: &str,
    captured: &PodCapture,
) -> String {
    loop {
        if let Some(call) = dispatch.dequeue_call(tool, job_id).await {
            *captured.lock().unwrap() = Some(call.call_id.clone());
            return call.call_id;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

pub(crate) fn stdout_frame(s: &str) -> ToolResultFrame {
    ToolResultFrame {
        frame: Some(Frame::Stdout(s.into())),
    }
}

pub(crate) fn done_terminal() -> ToolResultFrame {
    ToolResultFrame {
        frame: Some(Frame::Complete(ToolComplete {
            outcome: ToolOutcome::Done as i32,
            exit_code: 0,
        })),
    }
}

/// Spawn a pod that claims the call for `(tool, job_id)`, then feeds `frames`
/// through `forward_result_frames`.
pub(crate) fn spawn_pod(
    dispatch: &Arc<DispatchState>,
    tool: &str,
    job_id: &str,
    frames: Vec<ToolResultFrame>,
) -> PodCapture {
    let captured: PodCapture = Arc::default();
    let pod_dispatch = dispatch.clone();
    let pod_captured = captured.clone();
    let tool = tool.to_string();
    let job_id = job_id.to_string();
    tokio::spawn(async move {
        let call_id = claim_call(&pod_dispatch, &tool, &job_id, &pod_captured).await;
        let _ = pod_dispatch
            .forward_result_frames(
                call_id,
                tokio_stream::iter(frames.into_iter().map(Ok::<ToolResultFrame, tonic::Status>)),
            )
            .await;
    });
    captured
}

/// Spawn a pod that claims the call for `(tool, job_id)` but streams no
/// result — used where only the cancel path ends the wait.
pub(crate) fn spawn_claiming_pod(
    dispatch: &Arc<DispatchState>,
    tool: &str,
    job_id: &str,
) -> PodCapture {
    let captured: PodCapture = Arc::default();
    let pod_dispatch = dispatch.clone();
    let pod_captured = captured.clone();
    let tool = tool.to_string();
    let job_id = job_id.to_string();
    tokio::spawn(async move {
        claim_call(&pod_dispatch, &tool, &job_id, &pod_captured).await;
    });
    captured
}

/// Block until the pod has claimed its call, returning the captured id.
pub(crate) async fn claimed(capture: &PodCapture) -> String {
    wait_until("the pod claims its call", || {
        capture.lock().unwrap().is_some()
    })
    .await;
    capture.lock().unwrap().clone().unwrap()
}
