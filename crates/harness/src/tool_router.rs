//! Tool router: fan-in across the live toolset table, the harness-local
//! runtime and the client's channel-stream tools.
//!
//! Every tool the LLM sees has a `Source`. `Toolset` tools come from the
//! toolsets this agent binds, rebuilt from the watched toolset ConfigMaps on
//! every change, and dispatch in-process via `DispatchState`, which spawns the
//! tool Job.
//! `Runtime` tools (`runtime_tools`) dispatch in-process.
//! `ChannelStream` tools (`channel_stream_tools`) run on the client over the
//! relay.
//!
//! A model call to a toolset tool is put to the user for approval before it
//! runs (`tool_approval`). Client-direct calls are not.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use proto_common::{text_block, CallToolResponse, ToolDefinition, ToolInfo, ToolResultFrame};
use shared::toolset::{validate_call_input, ArgDecl, ToolsetConfig, ToolsetEntry, ToolsetSecret};
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

use tokio_util::sync::CancellationToken;

use crate::agent_state::AgentState;
use crate::channel_stream_tools;
use crate::clients::RelayRpc;
use crate::dispatch::DispatchState;
use crate::dispatch_hub::DispatchHub;
use crate::execution_log::{assemble_from_frames, frames_from_response, ExecutionLogWriter};
use crate::instructions::Instructions;
use crate::runtime_tools::{self, DispatchAbort};
use crate::tool_approval::{self, Approval};

/// Mints a fresh relay handle per call.
pub(crate) type RelaySeam = Arc<dyn Fn() -> Box<dyn RelayRpc> + Send + Sync>;

/// Which subsystem owns a given tool name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Toolset,
    Runtime,
    ChannelStream,
}

/// Tool dispatch surface used by the agent loop.
#[async_trait::async_trait]
pub(crate) trait ToolDispatcher: Send + Sync {
    async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        has_client: bool,
        tool_call_id: &str,
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort>;
}

/// One catalog element: the LLM-facing tool `info`, its `source`, and — for
/// `Toolset` tools — the declared `args` carrying each arg's `env` mapping. The
/// three swap atomically so a tool's `env` can never lag its definition. Only
/// the in-process dispatch producer reads `args`; the outbound projections read
/// `info` alone, so `env` never leaves the cluster.
#[derive(Clone)]
struct CatalogEntry {
    info: ToolInfo,
    source: Source,
    args: Vec<ArgDecl>,
    /// For a `Toolset` tool, every Secret its toolset is given; each call of
    /// the tool gets all of them. Empty for runtime and channel-stream tools.
    /// Rides the same atomic swap as `args`, so a tool's Secrets can never lag
    /// its definition.
    secrets: Vec<ToolsetSecret>,
}

pub(crate) struct ToolRouter {
    /// This agent's instructions reader, backing the in-process `Runtime` arm
    /// (`ReadInstructions`/`ListInstructions` content). Reads the mounted
    /// read-only instructions volume; no network hop.
    instructions: Arc<Instructions>,
    /// This harness's own agent name. Each harness serves only its own
    /// agent's instructions; the name roots every instructions read.
    agent: String,
    /// Dialer for the relay gateway's internal listener. `ChannelStream`-source
    /// tools push `ServerRequest` frames through it, and approval requests go
    /// out on it. `None` when no gateway is configured.
    relay: Option<RelaySeam>,
    agent_state: Arc<AgentState>,
    /// Live catalog keyed by tool name: runtime and channel-stream tools from
    /// `new`, toolset tools rebuilt by `apply_toolsets`. Lock-free reads via
    /// `ArcSwap`; writers serialize through `apply_lock`.
    tools: ArcSwap<Vec<CatalogEntry>>,
    /// Serializes `apply_toolsets` so concurrent read-modify-swap can't drop
    /// an update. No `.await` crosses the guard, so `std::sync::Mutex` is
    /// correct.
    apply_lock: std::sync::Mutex<()>,
    /// The toolsets this agent binds, in order. A toolset outside it is
    /// never listed.
    toolset_scope: Vec<String>,
    /// Each bound toolset's Secrets, keyed by toolset.
    toolset_secrets: HashMap<String, Vec<ToolsetSecret>>,
    /// Live client-facing tool-call sessions, keyed by `call_id`. Minted on
    /// `dispatch_client_tool`, retired when the call's frame stream ends.
    /// A present entry means the call is in flight (cancelable); an absent one
    /// is finished (served from the execution log) or never dispatched.
    calls: Arc<RwLock<HashMap<String, CallSessionHandle>>>,
    /// In-process tool-call dispatcher. When set, `Source::Toolset` agent-turn
    /// calls spawn the tool Job and drain its result here. `None` leaves the
    /// Toolset arm answering "tool dispatch is not configured".
    dispatch: Option<Arc<DispatchState>>,
    /// The agent's sub-agents, backing `StartSubAgent`. `None` in a
    /// sub-agent's own harness.
    hub: Option<Arc<DispatchHub>>,
}

/// One live dispatch/await/cancel session's shared state. Its one
/// `CallPublisher` appends each frame to the execution log AND publishes it to
/// `sender` (live fan-out) while recording it in `frames` (replay-so-far). A
/// late `await` subscriber snapshots `frames` and subscribes to `sender` under
/// the same lock so no frame slips between the snapshot and the subscription.
#[derive(Clone)]
struct CallSessionHandle {
    sender: broadcast::Sender<ToolResultFrame>,
    frames: Arc<std::sync::Mutex<Vec<ToolResultFrame>>>,
}

/// The publishing side of one live client call session, from
/// [`ToolRouter::open_client_call`].
struct CallPublisher {
    call_id: String,
    exec: Arc<dyn ExecutionLogWriter>,
    calls: Arc<RwLock<HashMap<String, CallSessionHandle>>>,
    session: CallSessionHandle,
}

impl CallPublisher {
    /// Append `frame` to the execution log, then push it to the session's
    /// frames and send it on the fan-out.
    async fn publish(&self, frame: ToolResultFrame) {
        append_or_warn(&*self.exec, &self.call_id, &frame).await;
        // Push then broadcast as ONE critical section: a late subscriber that
        // takes this lock either sees the frame in its snapshot (before push)
        // or on the fan-out (after send), never both.
        // `broadcast::Sender::send` is synchronous and non-blocking, so no
        // `.await` crosses the guard.
        let mut guard = self.session.frames.lock().unwrap();
        guard.push(frame.clone());
        let _ = self.session.sender.send(frame);
    }

    /// Retire the session: a re-subscriber is served from the persisted
    /// execution log, not this live handle.
    async fn retire(self) {
        self.calls.write().await.remove(&self.call_id);
    }
}

/// Append `frame` to the execution log. A failed append is logged, not fatal.
async fn append_or_warn(exec: &dyn ExecutionLogWriter, call_id: &str, frame: &ToolResultFrame) {
    if let Err(e) = exec.append_frame(call_id, frame).await {
        tracing::warn!(error = %e, call_id = %call_id, "failed to append execution frame");
    }
}

/// Why a toolset cannot be listed: a tool whose name is already listed, or an
/// arg mapped to an env the harness reserves on tool jobs.
fn clash(entry: &ToolsetEntry, listed: &[String]) -> Option<String> {
    for tool in &entry.tools {
        if listed.contains(&tool.name) {
            return Some(format!("tool {} is already listed", tool.name));
        }
        if let Some(arg) = tool
            .args
            .iter()
            .find(|a| crate::job::is_reserved_env(&a.env))
        {
            return Some(format!(
                "tool {} arg {} maps to reserved env {}",
                tool.name, arg.name, arg.env
            ));
        }
    }
    None
}

/// Whether a frame is the terminal `ToolComplete`.
pub(crate) fn is_terminal_frame(frame: &ToolResultFrame) -> bool {
    matches!(
        frame.frame,
        Some(proto_common::tool_result_frame::Frame::Complete(_))
    )
}

/// A synthetic FAILED terminal for a call whose frame stream ended without one:
/// a live stream that closed early, or a truncated persisted record.
fn failed_terminal() -> ToolResultFrame {
    ToolResultFrame {
        frame: Some(proto_common::tool_result_frame::Frame::Complete(
            proto_common::ToolComplete {
                outcome: proto_common::ToolOutcome::Failed as i32,
                exit_code: -1,
            },
        )),
    }
}

pub(crate) fn relay_seam<R: RelayRpc + Clone + Sync + 'static>(relay: R) -> RelaySeam {
    Arc::new(move || Box::new(relay.clone()) as Box<dyn RelayRpc>)
}

impl ToolRouter {
    pub(crate) fn new(
        instructions: Arc<Instructions>,
        agent: String,
        relay: Option<RelaySeam>,
        agent_state: Arc<AgentState>,
    ) -> Self {
        let mut tools: Vec<CatalogEntry> = runtime_tools::tool_definitions()
            .into_iter()
            .map(|t| CatalogEntry {
                info: t,
                source: Source::Runtime,
                args: Vec::new(),
                secrets: Vec::new(),
            })
            .collect();
        tools.extend(
            channel_stream_tools::tool_definitions()
                .into_iter()
                .map(|t| CatalogEntry {
                    info: t,
                    source: Source::ChannelStream,
                    args: Vec::new(),
                    secrets: Vec::new(),
                }),
        );
        let mut seen = std::collections::HashSet::new();
        for entry in &tools {
            assert!(
                seen.insert(entry.info.name.clone()),
                "duplicate framework tool: {}",
                entry.info.name
            );
        }
        // Sort for deterministic advertisement order.
        tools.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        Self {
            instructions,
            agent,
            relay,
            agent_state,
            tools: ArcSwap::new(Arc::new(tools)),
            apply_lock: std::sync::Mutex::new(()),
            toolset_scope: Vec::new(),
            toolset_secrets: HashMap::new(),
            calls: Arc::new(RwLock::new(HashMap::new())),
            dispatch: None,
            hub: None,
        }
    }

    /// Install `relay` as the relay every call reaches, in place of the one
    /// given to `new`.
    #[cfg(test)]
    fn with_relay<R: RelayRpc + Clone + Sync + 'static>(mut self, relay: R) -> Self {
        self.relay = Some(relay_seam(relay));
        self
    }

    fn relay_rpc(&self) -> Option<Box<dyn RelayRpc>> {
        self.relay.as_ref().map(|mint| mint())
    }

    /// Attach the in-process tool-call dispatcher. Production wires this at boot
    /// so `Source::Toolset` agent-turn calls run locally.
    pub(crate) fn with_dispatch(mut self, dispatch: Arc<DispatchState>) -> Self {
        self.dispatch = Some(dispatch);
        self
    }

    /// Offer the sub-agent tools, backed by `hub`.
    pub(crate) fn with_sub_agents(mut self, hub: Arc<DispatchHub>) -> Self {
        let mut tools = (**self.tools.load()).clone();
        tools.extend(
            runtime_tools::sub_agent_definitions()
                .into_iter()
                .map(|info| CatalogEntry {
                    info,
                    source: Source::Runtime,
                    args: Vec::new(),
                    secrets: Vec::new(),
                }),
        );
        tools.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        self.tools.store(Arc::new(tools));
        self.hub = Some(hub);
        self
    }

    fn execution_log(&self) -> Arc<dyn ExecutionLogWriter> {
        self.agent_state.execution_log()
    }

    /// Bind this agent's toolsets and each toolset's Secrets.
    pub(crate) fn with_toolset_scope(
        mut self,
        toolsets: Vec<String>,
        secrets: HashMap<String, Vec<ToolsetSecret>>,
    ) -> Self {
        self.toolset_scope = toolsets;
        self.toolset_secrets = secrets;
        self
    }

    /// Rebuild the toolset-owned catalog from the live toolset table, listing
    /// only the toolsets this agent binds, each tool carrying its args and
    /// its toolset's Secrets. A toolset that names an already-listed tool
    /// (built-in or from an earlier toolset), or maps an arg to an env the
    /// harness stamps on tool jobs, is left out whole and the returned warnings
    /// name it. Runtime and channel-stream entries are preserved.
    pub(crate) fn apply_toolsets(&self, table: &ToolsetConfig) -> Vec<String> {
        let _guard = self
            .apply_lock
            .lock()
            .expect("apply_lock poisoned — unrecoverable");
        let mut next: Vec<CatalogEntry> = self
            .tools
            .load()
            .iter()
            .filter(|e| e.source != Source::Toolset)
            .cloned()
            .collect();
        let mut listed: Vec<String> = next.iter().map(|e| e.info.name.clone()).collect();
        let mut warnings = Vec::new();
        for name in &self.toolset_scope {
            let Some(entry) = table.get(name) else {
                continue;
            };
            if let Some(reason) = clash(entry, &listed) {
                warnings.push(format!("toolset {name} left out: {reason}"));
                continue;
            }
            listed.extend(entry.tools.iter().map(|t| t.name.clone()));
            let secrets = self.toolset_secrets.get(name).cloned().unwrap_or_default();
            next.extend(entry.tools.iter().map(|t| CatalogEntry {
                info: ToolInfo {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters_json: t.parameters_json(),
                    toolset: name.clone(),
                },
                source: Source::Toolset,
                args: t.args.clone(),
                secrets: secrets.clone(),
            }));
        }
        next.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        let toolset_tools: Vec<&str> = next
            .iter()
            .filter(|e| e.source == Source::Toolset)
            .map(|e| e.info.name.as_str())
            .collect();
        tracing::info!(count = next.len(), tools = ?toolset_tools, "tool router refreshed");
        self.tools.store(Arc::new(next));
        warnings
    }

    pub(crate) fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .load()
            .iter()
            .map(|e| ToolDefinition {
                name: e.info.name.clone(),
                description: e.info.description.clone(),
                parameters_json: e.info.parameters_json.clone(),
            })
            .collect()
    }

    fn source_of(&self, name: &str) -> Option<Source> {
        self.tools
            .load()
            .iter()
            .find(|e| e.info.name == name)
            .map(|e| e.source)
    }

    /// The named tool's catalog entry, from one snapshot, so its toolset,
    /// args and Secrets always belong together.
    fn entry(&self, name: &str) -> Option<CatalogEntry> {
        self.tools
            .load()
            .iter()
            .find(|e| e.info.name == name)
            .cloned()
    }

    /// Convert the model's arg-name-keyed input into the env-keyed map the
    /// tool-job pod runs, then begin the call with every Secret of the tool's
    /// toolset.
    async fn begin_toolset_call(
        &self,
        dispatch: &Arc<DispatchState>,
        name: &str,
        input_json: &str,
    ) -> Result<(String, mpsc::Receiver<ToolResultFrame>), String> {
        let entry = self
            .entry(name)
            .ok_or_else(|| format!("no toolset for tool {name}"))?;
        let env_args = validate_call_input(input_json, &entry.args).map_err(|e| e.to_string())?;
        dispatch
            .begin_call(name, &entry.info.toolset, env_args, &entry.secrets)
            .await
    }

    pub(crate) async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        has_client: bool,
        tool_call_id: &str,
        // The parent turn's cancel token. The approval request and the
        // `Toolset` and `ChannelStream` arms race it against their wait; a
        // `Runtime` tool ignores it.
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort> {
        let source = self
            .source_of(name)
            .ok_or_else(|| DispatchAbort::Error(format!("unknown tool: {name}")))?;
        // The agent's own message and stop to its sub-agent ask no approval.
        let unasked = source == Source::Runtime
            && [
                runtime_tools::MESSAGE_SUB_AGENT_TOOL_NAME,
                runtime_tools::STOP_SUB_AGENT_TOOL_NAME,
            ]
            .contains(&name);
        if !unasked {
            if let Approval::Refused(refusal) = self.ask_approval(name, input_json, cancel).await? {
                return Ok(refusal);
            }
        }
        match source {
            Source::Toolset => {
                let dispatch = self.dispatch.clone().ok_or_else(|| {
                    DispatchAbort::Error("tool dispatch is not configured".into())
                })?;
                self.dispatch_toolset_in_process(dispatch, name, input_json, cancel)
                    .await
            }
            Source::Runtime => match &self.hub {
                Some(hub) if runtime_tools::SUB_AGENT_TOOLS.contains(&name) => {
                    Ok(runtime_tools::dispatch_sub_agent_tool(name, input_json, hub).await)
                }
                _ => runtime_tools::dispatch(
                    name,
                    input_json,
                    &self.instructions,
                    &self.agent,
                    &self.agent_state,
                )
                .await
                .map_err(DispatchAbort::Error),
            },
            Source::ChannelStream => {
                let mut gateway = self.relay_rpc().ok_or_else(|| {
                    DispatchAbort::Error("relay gateway client not configured".into())
                })?;
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(DispatchAbort::Cancelled),
                    outcome = channel_stream_tools::dispatch(
                        name,
                        input_json,
                        &mut *gateway,
                        has_client,
                        tool_call_id,
                    ) => outcome.map_err(DispatchAbort::Error),
                }
            }
        }
    }

    async fn ask_approval(
        &self,
        name: &str,
        input_json: &str,
        cancel: &CancellationToken,
    ) -> Result<Approval, DispatchAbort> {
        let mut relay = self.relay_rpc();
        tool_approval::ask(
            relay.as_mut().map(|r| &mut **r as &mut dyn RelayRpc),
            name,
            input_json,
            cancel,
        )
        .await
    }

    /// Agent-turn dispatch of a `Source::Toolset` tool: begin the call, then
    /// drain its result.
    async fn dispatch_toolset_in_process(
        &self,
        dispatch: Arc<DispatchState>,
        name: &str,
        input_json: &str,
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort> {
        let (call_id, result_rx) = self
            .begin_toolset_call(&dispatch, name, input_json)
            .await
            .map_err(DispatchAbort::Error)?;

        let exec = self.execution_log();

        // Drain the tool job's ordered frame stream in a detached task,
        // appending each frame to the execution log as it arrives so the
        // agent-turn call is re-subscribable from the same record the client
        // path writes. The task outlives a cancel, so the Canceled terminal
        // still lands in the log rather than being dropped with the abandoned
        // future. Append failures are non-fatal.
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let drain_call_id = call_id.clone();
        tokio::spawn(async move {
            let mut result_rx = result_rx;
            let mut frames = Vec::new();
            let mut terminal_seen = false;
            while let Some(frame) = result_rx.recv().await {
                terminal_seen = is_terminal_frame(&frame);
                append_or_warn(&*exec, &drain_call_id, &frame).await;
                frames.push(frame);
                if terminal_seen {
                    break;
                }
            }
            // The stream closed with no terminal: fail the call, as
            // `await_client_tool` does, rather than return partial output as a
            // success.
            if !terminal_seen {
                let failed = failed_terminal();
                append_or_warn(&*exec, &drain_call_id, &failed).await;
                frames.push(failed);
            }
            let _ = done_tx.send(frames);
        });

        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Fire the per-call cancel so the dial driver sends the cancel
                // on the pod's stream; the drain task keeps running to record the
                // terminal. The turn returns Cancelled at once.
                dispatch.fire_call_cancel(&call_id).await;
                Err(DispatchAbort::Cancelled)
            }
            res = done_rx => {
                let frames = res.map_err(|_| {
                    DispatchAbort::Error("dispatch drain task ended without a result".into())
                })?;
                Ok(assemble_from_frames(&frames))
            }
        }
    }

    /// Client-facing dispatch. The harness is the authority for every tool: it
    /// decides what exists and what runs, and the only thing a tool's `Source`
    /// varies is WHERE the work happens. So this resolves the source first, the
    /// same way the agent-turn `call_tool` does, and only then chooses between
    /// spawning the tool's Job and running the tool in-process. Either way the
    /// caller gets a `call_id` back before the call resolves, and awaits its
    /// frames on `await_client_tool`.
    ///
    /// An unknown name is answered here, before anything is spawned.
    pub(crate) async fn dispatch_client_tool(
        &self,
        name: &str,
        input_json: &str,
    ) -> Result<String, String> {
        match self
            .source_of(name)
            .ok_or_else(|| format!("unknown tool: {name}"))?
        {
            Source::Toolset => self.dispatch_client_toolset_tool(name, input_json).await,
            Source::Runtime => self.dispatch_client_runtime_tool(name, input_json).await,
            // A channel-stream tool executes on the user's own device via a
            // `ServerRequest`. Dispatching one FROM that device would be a
            // request looping back at its own sender, so it is refused by name.
            Source::ChannelStream => Err(format!(
                "{name} is not client-dispatchable: channel-stream tools execute on the client itself"
            )),
        }
    }

    /// Begin the toolset call, register a live session, spawn its one consumer,
    /// and return the id before the call resolves. The consumer appends and
    /// publishes every frame, stays subscribed through a cancel, and retires the
    /// session when the frame stream ends.
    async fn dispatch_client_toolset_tool(
        &self,
        name: &str,
        input_json: &str,
    ) -> Result<String, String> {
        let dispatch = self
            .dispatch
            .clone()
            .ok_or_else(|| "tool dispatch is not configured".to_string())?;
        let (call_id, mut result_rx) = self.begin_toolset_call(&dispatch, name, input_json).await?;

        let publisher = self.open_client_call(&call_id).await;
        tokio::spawn(async move {
            while let Some(frame) = result_rx.recv().await {
                let terminal = is_terminal_frame(&frame);
                publisher.publish(frame).await;
                if terminal {
                    break;
                }
            }
            publisher.retire().await;
        });

        Ok(call_id)
    }

    /// Register a live session for `call_id` and return the publisher its
    /// one producer feeds.
    async fn open_client_call(&self, call_id: &str) -> CallPublisher {
        let (sender, _) = broadcast::channel(256);
        let session = CallSessionHandle {
            sender,
            frames: Arc::new(std::sync::Mutex::new(Vec::new())),
        };
        self.calls
            .write()
            .await
            .insert(call_id.to_string(), session.clone());
        CallPublisher {
            call_id: call_id.to_string(),
            exec: self.execution_log(),
            calls: self.calls.clone(),
            session,
        }
    }

    /// Run a `Runtime`-source tool in-process for the client, giving it the same
    /// call_id / frame-stream / execution-log contract a toolset call has.
    ///
    /// The dispatch runs in a spawned task rather than inline so the caller gets
    /// its id immediately. The id is minted here, and the `rt-` prefix marks
    /// that provenance in `execution.json`.
    async fn dispatch_client_runtime_tool(
        &self,
        name: &str,
        input_json: &str,
    ) -> Result<String, String> {
        let call_id = format!("rt-{}", uuid::Uuid::new_v4());

        let publisher = self.open_client_call(&call_id).await;
        let instructions = self.instructions.clone();
        let agent = self.agent.clone();
        let agent_state = self.agent_state.clone();
        // A person may use every sub-agent tool but `StartSubAgent`.
        let hub = self
            .hub
            .clone()
            .filter(|_| name != runtime_tools::START_SUB_AGENT_TOOL_NAME);
        let name = name.to_string();
        let input_json = input_json.to_string();

        tokio::spawn(async move {
            let dispatched = match hub {
                Some(hub) if runtime_tools::SUB_AGENT_TOOLS.contains(&name.as_str()) => {
                    Ok(runtime_tools::dispatch_sub_agent_tool(&name, &input_json, &hub).await)
                }
                _ => {
                    runtime_tools::dispatch(&name, &input_json, &instructions, &agent, &agent_state)
                        .await
                }
            };
            let response = match dispatched {
                Ok(response) => response,
                Err(e) => CallToolResponse {
                    content: vec![text_block(format!("{name} error: {e}"))],
                    is_error: true,
                },
            };

            for frame in frames_from_response(&response) {
                publisher.publish(frame).await;
            }
            publisher.retire().await;
        });

        Ok(call_id)
    }

    /// Client-facing await: serve the call's frames as a live stream. A live
    /// session replays the frames seen so far then continues on its fan-out to
    /// the terminal; a finished call is replayed from the persisted execution
    /// log; an unknown call_id is `NotFound`.
    pub(crate) async fn await_client_tool(
        &self,
        call_id: &str,
    ) -> Result<ReceiverStream<Result<ToolResultFrame, Status>>, Status> {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let handle = self.calls.read().await.get(call_id).cloned();
        match handle {
            Some(h) => {
                // Snapshot the frames seen so far and subscribe under the same
                // lock the consumer holds around push+send, so the boundary is
                // clean: the snapshot is every frame sent so far, the
                // subscription every frame sent after — no overlap, no gap.
                let (snapshot, mut sub) = {
                    let guard = h.frames.lock().unwrap();
                    (guard.clone(), h.sender.subscribe())
                };
                let frames = h.frames.clone();
                tokio::spawn(async move {
                    // The snapshot is the first batch; then follow the live
                    // fan-out to the terminal. A subscriber that lagged refills
                    // the skipped frames from `frames`, which holds every frame
                    // sent. Any other recv error (the sender dropped without a
                    // terminal because the consumer retired on a stream error)
                    // ends the loop.
                    let mut terminal_seen = false;
                    let mut sent = 0;
                    let mut batch = snapshot;
                    loop {
                        for frame in batch {
                            let terminal = is_terminal_frame(&frame);
                            if tx.send(Ok(frame)).await.is_err() {
                                return;
                            }
                            sent += 1;
                            if terminal {
                                terminal_seen = true;
                                break;
                            }
                        }
                        if terminal_seen {
                            break;
                        }
                        batch = match sub.recv().await {
                            Ok(frame) => vec![frame],
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                let all = frames.lock().unwrap();
                                let end = (sent + n as usize).min(all.len());
                                all[sent..end].to_vec()
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        };
                    }
                    // The fan-out ended with no terminal: the call's stream died
                    // mid-flight. Close the client's await on a synthetic FAILED
                    // terminal so every client-driven call ends in an outcome.
                    if !terminal_seen {
                        let _ = tx.send(Ok(failed_terminal())).await;
                    }
                });
                Ok(ReceiverStream::new(rx))
            }
            None => {
                // The live session is gone. Resolve the call's persisted record
                // from the durable execution log, read by id. The log lives on
                // disk, not in an in-memory table, so resolution survives a
                // harness restart.
                let persisted = self.execution_log().read(call_id).await;
                match persisted {
                    Some(call) => {
                        let frames = call.frames().to_vec();
                        // A record without a terminal was dropped mid-call (the
                        // session died before the runtime's terminal); replay
                        // what persisted regardless, but note it.
                        let has_terminal = call.has_terminal();
                        tracing::debug!(
                            call_id,
                            complete = has_terminal,
                            replayed = frames.len(),
                            "serving await from the persisted execution record"
                        );
                        tokio::spawn(async move {
                            for frame in frames {
                                if tx.send(Ok(frame)).await.is_err() {
                                    return;
                                }
                            }
                            // A truncated record carries no terminal of its own.
                            // Close the client's await on a synthetic FAILED
                            // terminal so the replay still ends in an outcome.
                            if !has_terminal {
                                let _ = tx.send(Ok(failed_terminal())).await;
                            }
                        });
                        Ok(ReceiverStream::new(rx))
                    }
                    None => Err(Status::not_found(
                        "no in-flight or persisted call for call_id",
                    )),
                }
            }
        }
    }

    /// Client-facing cancel: route the per-call cancel into the in-process
    /// dispatcher only when the call is still in flight, and return whether a
    /// cancel was fired. An unknown or already-retired call_id is answered here
    /// — no cancel is fired — and reports that no call was canceled.
    pub(crate) async fn cancel_client_tool(&self, call_id: &str) -> bool {
        if !self.calls.read().await.contains_key(call_id) {
            return false;
        }
        let dispatch = match self.dispatch.clone() {
            Some(d) => d,
            None => return false,
        };
        dispatch.fire_call_cancel(call_id).await
    }
}

#[async_trait::async_trait]
impl ToolDispatcher for ToolRouter {
    async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        has_client: bool,
        tool_call_id: &str,
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort> {
        ToolRouter::call_tool(self, name, input_json, has_client, tool_call_id, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_doubles::{
        claimed, done_terminal, in_process_dispatch, seed, seed_job, spawn_claiming_pod, spawn_pod,
        stdout_frame, t, test_instructions, write_md, RelayAnswer, ScriptedRelay, TEST_AG,
    };
    use shared::toolset::ToolDecl;
    use tokio_stream::StreamExt;

    fn test_agent_state() -> Arc<AgentState> {
        Arc::new(crate::test_doubles::empty_agent_state())
    }

    fn empty_router() -> ToolRouter {
        ToolRouter::new(
            test_instructions(),
            TEST_AG.to_string(),
            None,
            test_agent_state(),
        )
    }

    /// An empty router whose agent state is rooted at `root`, so a test reads
    /// the `execution.json` its calls write.
    fn router_over(root: &std::path::Path) -> ToolRouter {
        ToolRouter::new(
            test_instructions(),
            TEST_AG.to_string(),
            None,
            Arc::new(crate::test_doubles::open_agent_state(root.to_path_buf())),
        )
    }

    fn names(router: &ToolRouter) -> Vec<String> {
        router
            .tool_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    #[test]
    fn applied_toolset_preserves_runtime_entries() {
        let router = seed(empty_router(), vec![t("Bash"), t("Git")]);
        let names = names(&router);
        assert!(names.iter().any(|n| n == "ReadInstructions"));
        assert!(names.iter().any(|n| n == "Bash"));
        assert!(names.iter().any(|n| n == "Git"));
    }

    fn table(toolsets: Vec<(&str, Vec<ToolDecl>)>) -> ToolsetConfig {
        ToolsetConfig::from_map(
            toolsets
                .into_iter()
                .map(|(name, tools)| {
                    let entry = ToolsetEntry {
                        tools,
                        ..Default::default()
                    };
                    (name.to_string(), entry)
                })
                .collect(),
        )
    }

    fn scoped_router(toolsets: &[&str]) -> ToolRouter {
        empty_router().with_toolset_scope(
            toolsets.iter().map(|s| s.to_string()).collect(),
            HashMap::<String, Vec<ToolsetSecret>>::new(),
        )
    }

    #[test]
    fn toolset_naming_a_builtin_tool_is_excluded_and_named() {
        let router = scoped_router(&["clash", "git"]);

        let warnings = router.apply_toolsets(&table(vec![
            ("clash", vec![t("ReadInstructions"), t("Other")]),
            ("git", vec![t("git-status")]),
        ]));

        assert_eq!(
            warnings,
            ["toolset clash left out: tool ReadInstructions is already listed"]
        );
        let names = names(&router);
        assert!(names.iter().any(|n| n == "git-status"));
        assert!(!names.iter().any(|n| n == "Other"));
        assert_eq!(names.iter().filter(|n| *n == "ReadInstructions").count(), 1);
        assert_eq!(router.source_of("ReadInstructions"), Some(Source::Runtime));
    }

    #[test]
    fn toolset_naming_a_tool_an_earlier_toolset_listed_is_excluded_and_named() {
        let router = scoped_router(&["a", "b"]);

        let warnings =
            router.apply_toolsets(&table(vec![("a", vec![t("X")]), ("b", vec![t("X")])]));

        assert_eq!(warnings, ["toolset b left out: tool X is already listed"]);
        let tools = router.tools.load();
        let x: Vec<&CatalogEntry> = tools.iter().filter(|e| e.info.name == "X").collect();
        assert_eq!(x.len(), 1);
        assert_eq!(x[0].info.toolset, "a");
    }

    #[test]
    fn toolset_mapping_an_arg_to_reserved_env_is_excluded_and_named() {
        let router = scoped_router(&["fetch", "git"]);

        let warnings = router.apply_toolsets(&table(vec![
            ("fetch", vec![tool_with_arg("Fetch", "url", "HOME")]),
            ("git", vec![t("git-status")]),
        ]));

        assert_eq!(
            warnings,
            ["toolset fetch left out: tool Fetch arg url maps to reserved env HOME"]
        );
        let names = names(&router);
        assert!(names.iter().any(|n| n == "git-status"));
        assert!(!names.iter().any(|n| n == "Fetch"));
    }

    #[test]
    fn toolset_added_after_first_apply_is_listed() {
        let router = scoped_router(&["git", "extra"]);
        router.apply_toolsets(&table(vec![("git", vec![t("git-status")])]));
        assert!(!names(&router).iter().any(|n| n == "Extra"));

        let warnings = router.apply_toolsets(&table(vec![
            ("git", vec![t("git-status")]),
            ("extra", vec![t("Extra")]),
        ]));

        assert!(warnings.is_empty(), "{warnings:?}");
        let names = names(&router);
        assert!(names.iter().any(|n| n == "Extra"));
        assert!(names.iter().any(|n| n == "git-status"));
    }

    #[test]
    fn toolset_outside_scope_is_never_listed() {
        let router = scoped_router(&["git"]);

        router.apply_toolsets(&table(vec![
            ("git", vec![t("git-status")]),
            ("other", vec![t("Other")]),
        ]));

        let names = names(&router);
        assert!(names.iter().any(|n| n == "git-status"));
        assert!(!names.iter().any(|n| n == "Other"));
    }

    fn tool_with_arg(name: &str, arg_name: &str, env: &str) -> ToolDecl {
        ToolDecl {
            name: name.into(),
            description: format!("desc:{name}"),
            args: vec![ArgDecl {
                name: arg_name.into(),
                ty: shared::toolset::ArgType::String,
                required: true,
                env: env.into(),
                description: None,
            }],
        }
    }

    #[test]
    fn outbound_definitions_carry_no_arg_env() {
        let router = seed(
            empty_router(),
            vec![tool_with_arg("Git", "message", "SECRET_ENV")],
        );
        for def in router.tool_definitions() {
            assert!(!def.name.contains("SECRET_ENV"));
            assert!(!def.description.contains("SECRET_ENV"));
            assert!(
                !def.parameters_json.contains("SECRET_ENV"),
                "arg env must not reach the outbound tool-definition surface"
            );
        }
    }

    /// Assert a dispatch error carries `needle`. Routing errors surface as
    /// `DispatchAbort::Error`.
    fn assert_dispatch_error(err: DispatchAbort, needle: &str) {
        match err {
            DispatchAbort::Error(e) => assert!(
                e.contains(needle),
                "expected error containing {needle:?}, got {e:?}"
            ),
            DispatchAbort::Cancelled => panic!("expected Error({needle:?}), got Cancelled"),
        }
    }

    #[tokio::test]
    async fn a_router_not_given_the_hub_lists_no_start_sub_agent_and_calls_it_unknown() {
        let router = empty_router();
        assert!(!names(&router).iter().any(|n| n == "StartSubAgent"));
        let cancel = CancellationToken::new();
        let err = router
            .call_tool("StartSubAgent", "{}", false, "tc", &cancel)
            .await
            .unwrap_err();
        assert_dispatch_error(err, "unknown tool: StartSubAgent");
    }

    #[tokio::test]
    async fn call_tool_routes_list_instructions_through_in_process_runtime_dispatch() {
        // `ListInstructions` is Runtime-source, served in-process from the
        // instructions — no gRPC hop. Against an empty instructions it returns
        // an empty array (not an "unknown tool" or "not configured" error),
        // proving the call reached the in-process instructions reader.
        let router = empty_router().with_relay(ScriptedRelay::new(vec![RelayAnswer::approve()]));
        let cancel = CancellationToken::new();
        let resp = router
            .call_tool("ListInstructions", "{}", false, "tc", &cancel)
            .await
            .expect("ListInstructions routes to the in-process runtime dispatch");
        assert!(!resp.is_error);
        assert_eq!(proto_common::content_text(&resp.content), "[]");
    }

    #[test]
    fn source_of_returns_correct_attribution() {
        let router = seed(empty_router(), vec![t("Bash")]);
        assert_eq!(router.source_of("Bash"), Some(Source::Toolset));
        assert_eq!(router.source_of("ReadInstructions"), Some(Source::Runtime));
        assert_eq!(router.source_of("Ghost"), None);
    }

    // The `Source::Toolset` arm is the caller in the cascade: it learns the
    // call_id from `begin_call`, then races the drain's result against the
    // turn's cancel token. On cancel it fires exactly one `fire_call_cancel`
    // and returns the terminal `DispatchAbort::Cancelled`; uncancelled, it
    // returns the toolset result unchanged and fires no cancel.
    #[tokio::test]
    async fn toolset_cancel_fires_exactly_one_cancel_and_returns_cancelled() {
        // The pod claims the call but streams no terminal, so the wait pends
        // forever unless the fired cancel ends it. If the arm awaited the result
        // instead of racing (biased) the already-fired cancel, this test would
        // hang.
        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        // The token fires while the user approves the call: a token fired
        // before the call would stop it at the approval wait, before
        // `begin_call`, and the pod would never claim.
        let cancel = CancellationToken::new();
        let router: ToolRouter =
            empty_router()
                .with_dispatch(dispatch.clone())
                .with_relay(ScriptedRelay::new(vec![RelayAnswer::ApproveAndCancel(
                    cancel.clone(),
                )]));
        let router = seed(router, vec![t("Bash")]);

        let capture = spawn_claiming_pod(&dispatch, "Bash", "job-x");

        let outcome = router.call_tool("Bash", "{}", false, "tc", &cancel).await;

        // Cancelled is returned as-is, not folded into `Ok(is_error=true)`.
        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "a fired cancel on a Toolset call must return Cancelled, got {outcome:?}"
        );

        // The begun call's id, learned from the pod that claimed it. The arm
        // already fired the per-call cancel for exactly this id before returning,
        // so its token is consumed — re-firing it here finds nothing (false).
        let call_id = claimed(&capture).await;
        assert!(
            !dispatch.fire_call_cancel(&call_id).await,
            "exactly one cancel for the begun call's id must already have been fired"
        );
    }

    // The in-process dispatch arm converts the model's arg-name-keyed input into
    // the env-keyed map the tool-job pod runs: a tool declaring arg `message`
    // mapped to env `MESSAGE`, called with `{"message":"hi"}`, enqueues
    // `{"MESSAGE":"hi"}`, never the raw request JSON.
    #[tokio::test]
    async fn in_process_dispatch_enqueues_env_keyed_args() {
        use std::sync::Mutex as StdMutex;

        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Git", "job-x").await;

        let router: ToolRouter = empty_router()
            .with_dispatch(dispatch.clone())
            .with_relay(ScriptedRelay::new(vec![RelayAnswer::approve()]));
        let router = seed(router, vec![tool_with_arg("Git", "message", "MESSAGE")]);

        // The pod: dequeue the assignment for its job, capture the args, then
        // fire the cancel so `call_tool` unblocks and returns.
        let cancel = CancellationToken::new();
        let captured: Arc<StdMutex<Option<HashMap<String, String>>>> =
            Arc::new(StdMutex::new(None));
        let pod_dispatch = dispatch.clone();
        let pod_captured = captured.clone();
        let pod_cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                if let Some(call) = pod_dispatch.dequeue_call("Git", "job-x").await {
                    *pod_captured.lock().unwrap() = Some(call.args);
                    pod_cancel.cancel();
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        });

        let outcome = router
            .call_tool("Git", r#"{"message":"hi"}"#, false, "tc", &cancel)
            .await;
        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "the pod cancelled the call after capturing its args, got {outcome:?}"
        );

        let args = captured
            .lock()
            .unwrap()
            .clone()
            .expect("the pod must have dequeued the call");
        assert_eq!(
            args.get("MESSAGE").map(String::as_str),
            Some("hi"),
            "the enqueued args must be keyed by the declared env name"
        );
        assert!(
            !args.contains_key("message"),
            "the raw request key must not survive into the pod's args"
        );
    }

    // Every call of a toolset gets exactly that toolset's Secrets, from both
    // callers. The allowlist and the job gate are per agent, so only the router
    // keeps one toolset's Secrets off another toolset's pod; the recorded
    // toolset is the egress half (it becomes the pod's `sycophant.md/toolset`
    // label, which the per-toolset egress policy selects on).
    #[tokio::test]
    async fn each_toolset_gets_exactly_its_own_secrets_from_both_callers() {
        let dispatch = in_process_dispatch(&["ssh", "git"]);
        seed_job(&dispatch, "SshTool", "job-ssh").await;
        seed_job(&dispatch, "GitTool", "job-git").await;

        let secret = |name: &str, path: &str| ToolsetSecret {
            secret: name.to_string(),
            path: path.to_string(),
        };
        let router: ToolRouter = empty_router()
            .with_dispatch(dispatch.clone())
            .with_relay(ScriptedRelay::new(vec![
                RelayAnswer::approve(),
                RelayAnswer::approve(),
            ]))
            .with_toolset_scope(
                vec!["ssh".to_string(), "git".to_string()],
                HashMap::from([
                    (
                        "ssh".to_string(),
                        vec![
                            secret("demo-key", "/home/agent/.ssh/id_ed25519"),
                            secret("local-key", "/home/agent/.ssh/local"),
                        ],
                    ),
                    (
                        "git".to_string(),
                        vec![secret("git-key", "/home/agent/.git-credentials")],
                    ),
                ]),
            );
        let warnings = router.apply_toolsets(&table(vec![
            ("ssh", vec![t("SshTool")]),
            ("git", vec![t("GitTool")]),
        ]));
        assert!(warnings.is_empty(), "{warnings:?}");

        let cancel = CancellationToken::new();
        for (tool, job_id) in [("SshTool", "job-ssh"), ("GitTool", "job-git")] {
            let capture = spawn_pod(&dispatch, tool, job_id, vec![done_terminal()]);
            router
                .call_tool(tool, "{}", false, "tc-model", &cancel)
                .await
                .unwrap_or_else(|e| panic!("model call to {tool} completes: {e:?}"));
            claimed(&capture).await;

            let capture = spawn_pod(&dispatch, tool, job_id, vec![done_terminal()]);
            router
                .dispatch_client_tool(tool, "{}")
                .await
                .unwrap_or_else(|e| panic!("client-direct call to {tool} completes: {e:?}"));
            claimed(&capture).await;
        }

        let ssh = (
            "SshTool".to_string(),
            "ssh".to_string(),
            vec!["demo-key".to_string(), "local-key".to_string()],
        );
        let git = (
            "GitTool".to_string(),
            "git".to_string(),
            vec!["git-key".to_string()],
        );
        assert_eq!(
            dispatch.begun.lock().unwrap().clone(),
            vec![ssh.clone(), ssh, git.clone(), git],
            "each call begins with its own toolset and exactly that toolset's Secrets \
             (model call, then client-direct call, per tool)"
        );
    }

    // The agent-turn Toolset arm appends each consumed frame to the execution
    // log as it arrives, so an agent-turn call is re-subscribable from the same
    // `execution.json` record the client path writes — one store format, no separate
    // end-of-call write.
    #[tokio::test]
    async fn toolset_arm_appends_consumed_frames_to_the_execution_log() {
        use crate::execution_log::LocalFsExecutionLog;
        use proto_common::tool_result_frame::Frame;

        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let dir = tempfile::tempdir().unwrap();
        let log: Arc<dyn ExecutionLogWriter> =
            Arc::new(LocalFsExecutionLog::new(dir.path().to_path_buf()));
        let router: ToolRouter = router_over(dir.path())
            .with_dispatch(dispatch.clone())
            .with_relay(ScriptedRelay::new(vec![RelayAnswer::approve()]));
        let router = seed(router, vec![t("Bash")]);

        let capture = spawn_pod(
            &dispatch,
            "Bash",
            "job-x",
            vec![stdout_frame("agent-turn output"), done_terminal()],
        );

        let cancel = CancellationToken::new(); // never fired

        router
            .call_tool("Bash", "{}", false, "tc", &cancel)
            .await
            .expect("an uncancelled agent-turn call returns its result");

        let call_id = claimed(&capture).await;
        let persisted = log
            .read(&call_id)
            .await
            .expect("the agent-turn arm must persist the call's frames");
        assert!(
            persisted.frames().iter().any(
                |f| matches!(f.frame.as_ref(), Some(Frame::Stdout(s)) if s == "agent-turn output")
            ),
            "each consumed frame is appended to the execution log as it arrives"
        );
        assert!(
            persisted.has_terminal(),
            "the terminal frame is appended too, so the agent-turn record is complete"
        );
    }

    // A tool call canceled in flight appends a terminal record with outcome
    // CANCELED to execution.json.
    //
    // The toolset RUNTIME emits its OWN terminal `ToolComplete{outcome: Canceled}`
    // when it SIGKILLs a mid-flight child. The harness writes one itself only
    // when the call ends before the pod sends a terminal.
    // This test scripts the fake toolset to stream a partial stdout line then that
    // runtime CANCELED terminal, fires the turn's cancel, and asserts the runtime's
    // canceled terminal lands in `execution.json`.
    #[tokio::test]
    async fn canceled_in_flight_call_appends_a_canceled_terminal_to_the_execution_log() {
        use crate::execution_log::LocalFsExecutionLog;
        use proto_common::tool_result_frame::Frame;
        use proto_common::{ToolComplete, ToolOutcome};

        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let dir = tempfile::tempdir().unwrap();
        let log: Arc<dyn ExecutionLogWriter> =
            Arc::new(LocalFsExecutionLog::new(dir.path().to_path_buf()));
        // The token fires while the user approves the call: a token fired
        // before the call would stop it at the approval wait, before
        // `begin_call`, and nothing would be drained into the log.
        let cancel = CancellationToken::new();
        let router: ToolRouter = router_over(dir.path())
            .with_dispatch(dispatch.clone())
            .with_relay(ScriptedRelay::new(vec![RelayAnswer::ApproveAndCancel(
                cancel.clone(),
            )]));
        let router = seed(router, vec![t("Bash")]);

        // The runtime streams a partial output line, then — because its child was
        // SIGKILLed on the cancel — its own terminal with outcome Canceled. The
        // 137 exit_code is a provenance fingerprint: a fabricated terminal would
        // not carry it.
        let capture = spawn_pod(
            &dispatch,
            "Bash",
            "job-x",
            vec![
                stdout_frame("partial output before cancel"),
                ToolResultFrame {
                    frame: Some(Frame::Complete(ToolComplete {
                        outcome: ToolOutcome::Canceled as i32,
                        exit_code: 137,
                    })),
                },
            ],
        );

        let outcome = router.call_tool("Bash", "{}", false, "tc", &cancel).await;
        // The turn still unwinds promptly on cancel; the drain runs detached.
        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "a fired cancel on a Toolset call still returns Cancelled promptly, got {outcome:?}"
        );

        // The CANCELED terminal is appended by the detached drain, so poll for
        // it (bounded, like the sibling cancel test).
        let call_id = claimed(&capture).await;
        let mut persisted = None;
        for _ in 0..200 {
            if let Some(call) = log.read(&call_id).await {
                if call.has_terminal() {
                    persisted = Some(call);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let persisted = persisted.expect(
            "a canceled-in-flight call must append the runtime's CANCELED terminal to execution.json",
        );

        let terminal = persisted
            .frames()
            .iter()
            .find_map(|f| match f.frame.as_ref() {
                Some(Frame::Complete(c)) => Some(*c),
                _ => None,
            })
            .expect("the drained record ends in a terminal ToolComplete");
        assert_eq!(
            terminal.outcome(),
            ToolOutcome::Canceled,
            "the appended terminal records outcome CANCELED, not FAILED or DONE"
        );
        assert_eq!(
            terminal.exit_code, 137,
            "the appended terminal is the drained runtime frame (exit_code 137), not synthesized"
        );

        // The pre-terminal partial output was drained and appended too, not just
        // the terminal — the drain appends each frame it reads.
        assert!(
            persisted.frames().iter().any(
                |f| matches!(f.frame.as_ref(), Some(Frame::Stdout(s)) if s == "partial output before cancel")
            ),
            "the drain appends each frame it reads, including the partial output before cancel"
        );
    }

    // ---- Execution-log durability tests ----

    /// Injected execution-log writer that parks the FIRST frame's persist until
    /// the test releases it. Lets the ordering test hold the append open and
    /// observe that no live subscriber sees the frame while its persist is still
    /// in flight.
    struct OrderingWriter {
        release: Arc<tokio::sync::Notify>,
        gated: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl ExecutionLogWriter for OrderingWriter {
        async fn append_frame(
            &self,
            _call_id: &str,
            _frame: &ToolResultFrame,
        ) -> Result<(), String> {
            // The first append parks until released. The guarantee is that the
            // append fully precedes the fan-out send, so while this is parked a
            // live subscriber must observe nothing.
            if !self.gated.swap(true, std::sync::atomic::Ordering::SeqCst) {
                self.release.notified().await;
            }
            Ok(())
        }

        async fn read(&self, _call_id: &str) -> Option<crate::execution_log::PersistedCall> {
            None
        }
    }

    // `dispatch_client_tool` must append a frame to the execution log BEFORE the
    // session's fan-out delivers it to a live subscriber. Observed by injecting a
    // writer whose first-frame persist parks: while it is parked, the live await
    // stream must yield nothing.
    //
    // The post-release delivery assert proves the silence while parked was the
    // ordering gate, not a permanently dead stream.
    #[tokio::test]
    async fn frame_is_persisted_before_a_live_subscriber_can_observe_it() {
        use std::sync::atomic::AtomicBool;
        use tokio::sync::Notify;

        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let release = Arc::new(Notify::new());
        let writer: Arc<dyn ExecutionLogWriter> = Arc::new(OrderingWriter {
            release: release.clone(),
            gated: AtomicBool::new(false),
        });
        let router: ToolRouter = ToolRouter::new(
            test_instructions(),
            TEST_AG.to_string(),
            None,
            Arc::new(crate::test_doubles::empty_agent_state().with_execution_log(writer)),
        )
        .with_dispatch(dispatch.clone());
        // The client path resolves the tool's source, so the toolset tool under
        // test must be advertised — as it is in production, where the client can
        // only dispatch a tool the harness advertised to it.
        let router = seed(router, vec![t("Bash")]);

        // The pod streams one stdout frame then the terminal.
        spawn_pod(
            &dispatch,
            "Bash",
            "job-x",
            vec![stdout_frame("ORDERED"), done_terminal()],
        );

        let call_id = router
            .dispatch_client_tool("Bash", "{}")
            .await
            .expect("dispatch mints the call and spawns its stream consumer");
        let mut stream = router
            .await_client_tool(&call_id)
            .await
            .expect("await subscribes to the live session");

        // The consumer's first-frame persist is parked (unreleased). A live
        // subscriber must observe NOTHING: the append precedes the fan-out send.
        let observed =
            tokio::time::timeout(std::time::Duration::from_millis(300), stream.next()).await;
        assert!(
            observed.is_err(),
            "a live subscriber observed a frame before its persist completed: {observed:?}"
        );

        // Releasing the persist must then deliver that same frame,
        // proving the non-observation above was the ordering gate, not a dead
        // stream.
        release.notify_one();
        let delivered = tokio::time::timeout(std::time::Duration::from_millis(2000), stream.next())
            .await
            .expect("after the persist completes the frame is delivered")
            .expect("the live stream yields an item")
            .expect("the yielded item is a frame, not a status error");
        assert!(
            matches!(
                delivered.frame.as_ref(),
                Some(proto_common::tool_result_frame::Frame::Stdout(s)) if s == "ORDERED"
            ),
            "the delivered frame is the one whose persist was gated, got {delivered:?}"
        );
    }

    /// A subscriber that falls behind the session's fan-out still gets every
    /// frame, in order, ending in the terminal: the skipped frames refill from
    /// the session's frame record. 400 frames overrun the 256-slot fan-out plus
    /// the 64-slot client channel while the client stream goes unread.
    #[tokio::test]
    async fn a_lagging_subscriber_gets_every_frame_in_order() {
        use proto_common::tool_result_frame::Frame;

        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let router = seed(
            empty_router().with_dispatch(dispatch.clone()),
            vec![t("Bash")],
        );
        let capture = spawn_claiming_pod(&dispatch, "Bash", "job-x");

        let call_id = router
            .dispatch_client_tool("Bash", "{}")
            .await
            .expect("dispatch mints the call");
        let mut stream = router
            .await_client_tool(&call_id)
            .await
            .expect("await subscribes to the live session");
        assert_eq!(claimed(&capture).await, call_id);

        let sent: Vec<String> = (0..400).map(|i| format!("line-{i}")).collect();
        let mut frames: Vec<ToolResultFrame> =
            sent.iter().map(|s| stdout_frame(s.as_str())).collect();
        frames.push(done_terminal());
        dispatch
            .forward_result_frames(
                call_id,
                tokio_stream::iter(frames.into_iter().map(Ok::<ToolResultFrame, Status>)),
            )
            .await
            .unwrap();

        let mut got = Vec::new();
        let terminal = loop {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
                .await
                .expect("the stream keeps delivering")
                .expect("the stream ends only after its terminal")
                .expect("a frame, not a status error");
            match frame.frame {
                Some(Frame::Stdout(s)) => got.push(s),
                Some(Frame::Complete(c)) => break c,
                other => panic!("unexpected frame {other:?}"),
            }
        };
        assert_eq!(got, sent);
        assert_eq!(terminal.outcome(), proto_common::ToolOutcome::Done);
    }

    /// A pod stream that ends without a terminal while the call's frame channel
    /// is full still fails the call. The drain is stalled on its first append,
    /// so the 65 frames leave the 64-slot channel full and the guard's FAILED
    /// cannot be sent; the drain's own no-terminal check must fail the call.
    #[tokio::test]
    async fn a_lost_terminal_on_a_full_channel_still_fails_the_call() {
        use std::sync::atomic::AtomicBool;
        use tokio::sync::Notify;

        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let release = Arc::new(Notify::new());
        let writer: Arc<dyn ExecutionLogWriter> = Arc::new(OrderingWriter {
            release: release.clone(),
            gated: AtomicBool::new(false),
        });
        let router: ToolRouter = ToolRouter::new(
            test_instructions(),
            TEST_AG.to_string(),
            None,
            Arc::new(crate::test_doubles::empty_agent_state().with_execution_log(writer)),
        )
        .with_dispatch(dispatch.clone())
        .with_relay(ScriptedRelay::new(vec![RelayAnswer::approve()]));
        let router = seed(router, vec![t("Bash")]);
        let capture = spawn_claiming_pod(&dispatch, "Bash", "job-x");
        let cancel = CancellationToken::new();

        let pod = async {
            let call_id = claimed(&capture).await;
            let frames = (0..65)
                .map(|i| stdout_frame(&format!("line-{i}")))
                .map(Ok::<ToolResultFrame, Status>);
            dispatch
                .forward_result_frames(call_id, tokio_stream::iter(frames))
                .await
                .unwrap();
            release.notify_one();
        };
        let (outcome, ()) = tokio::join!(router.call_tool("Bash", "{}", false, "tc", &cancel), pod);

        let response = outcome.expect("the call resolves");
        assert!(response.is_error, "{response:?}");
    }

    // A stream that ended WITHOUT a terminal frame retains every frame it
    // received, and a re-subscribe replays all of them (then a synthetic terminal
    // so the client's await still ends in an outcome). The record is written with
    // two stdout frames and no terminal, modelling a call whose live session died
    // mid-stream.
    #[tokio::test]
    async fn a_stream_without_terminal_replays_its_frames_then_a_synthetic_failure() {
        use proto_common::tool_result_frame::Frame;

        let agent_state = test_agent_state();
        let writer = agent_state.execution_log();
        // Two frames received, then the stream died: no terminal was appended.
        writer
            .append_frame("call-truncated", &stdout_frame("RETAINED-1"))
            .await
            .unwrap();
        writer
            .append_frame("call-truncated", &stdout_frame("RETAINED-2"))
            .await
            .unwrap();

        // Fresh router: no dispatch happened here, so any in-memory dispatch-time
        // table is empty. Resolution must read the durable log.
        let router: ToolRouter =
            ToolRouter::new(test_instructions(), TEST_AG.to_string(), None, agent_state);
        let mut stream = router
            .await_client_tool("call-truncated")
            .await
            .expect("a died-mid-call record must resolve from disk and replay its retained frames");

        let mut texts = Vec::new();
        let mut terminal = None;
        while let Some(item) = stream.next().await {
            match item.expect("the replayed item is a frame").frame {
                Some(Frame::Stdout(s)) => texts.push(s),
                Some(Frame::Complete(c)) => terminal = Some(c),
                _ => {}
            }
        }
        assert!(
            texts.iter().any(|t| t == "RETAINED-1") && texts.iter().any(|t| t == "RETAINED-2"),
            "every frame received before the stream ended is retained and replayed, got {texts:?}"
        );
        let terminal = terminal.expect(
            "a truncated replay is closed with a synthetic terminal so the client's await ends in an outcome",
        );
        assert_eq!(
            terminal.outcome(),
            proto_common::ToolOutcome::Failed,
            "a truncated record must terminate the await on a synthetic FAILED terminal"
        );
        assert_eq!(
            terminal.exit_code, -1,
            "the synthetic FAILED terminal carries the sentinel exit_code -1"
        );
    }

    // When a call's live session has already ended, a re-subscribe resolves the
    // call from the durable execution log and replays the recorded frames —
    // including the recorded terminal for a completed call.
    #[tokio::test]
    async fn retired_session_resubscribe_resolves_from_disk_and_replays() {
        use proto_common::tool_result_frame::Frame;

        let agent_state = test_agent_state();
        let writer = agent_state.execution_log();
        writer
            .append_frame("call-done", &stdout_frame("DISK-REPLAY"))
            .await
            .unwrap();
        writer
            .append_frame("call-done", &done_terminal())
            .await
            .unwrap();

        let router: ToolRouter =
            ToolRouter::new(test_instructions(), TEST_AG.to_string(), None, agent_state);
        let mut stream = router.await_client_tool("call-done").await.expect(
            "a retired call must resolve from the durable execution log, not an in-memory table",
        );

        let mut texts = Vec::new();
        let mut saw_terminal = false;
        while let Some(item) = stream.next().await {
            match item.expect("the replayed item is a frame").frame {
                Some(Frame::Stdout(s)) => texts.push(s),
                Some(Frame::Complete(_)) => saw_terminal = true,
                _ => {}
            }
        }
        assert!(
            texts.iter().any(|t| t == "DISK-REPLAY"),
            "the recorded frames are replayed from the durable log, got {texts:?}"
        );
        assert!(saw_terminal, "the recorded terminal is replayed");
    }

    // A call_id with no live session and no persisted record was never seen by
    // this harness: the await answers NotFound rather than an empty stream.
    #[tokio::test]
    async fn awaiting_a_call_id_the_harness_never_saw_is_not_found() {
        let router = empty_router();
        let status = match router.await_client_tool("never-dispatched").await {
            Ok(_) => panic!("an unknown call_id must not resolve to a stream"),
            Err(status) => status,
        };
        assert_eq!(
            status.code(),
            tonic::Code::NotFound,
            "an unknown call_id must be NotFound, got {status:?}"
        );
    }

    // ---- Client-path source-attribution tests ----

    /// Drain a client await stream to end-of-stream and assemble the result the
    /// client would see, using the same fold the client applies.
    async fn drain_client_call(
        router: &ToolRouter,
        call_id: &str,
    ) -> (CallToolResponse, Vec<ToolResultFrame>) {
        let mut stream = router
            .await_client_tool(call_id)
            .await
            .expect("the dispatched call must be awaitable");
        let mut frames = Vec::new();
        while let Some(item) = stream.next().await {
            frames.push(item.expect("the streamed item is a frame, not a status error"));
        }
        (assemble_from_frames(&frames), frames)
    }

    /// A router over instructions holding one file, `skills/classify.md`.
    fn classify_router() -> ToolRouter {
        let root = tempfile::TempDir::new().unwrap().keep();
        write_md(
            &root,
            &format!("{TEST_AG}/skills/classify.md"),
            "# Classify\n\nDecide the doctype.\n",
        );
        ToolRouter::new(
            Arc::new(Instructions::new(root)),
            TEST_AG.to_string(),
            None,
            test_agent_state(),
        )
    }

    // The harness is the authority for every tool: it decides what exists and
    // what runs, and the only thing that varies is WHERE the work happens. A
    // `Runtime`-source tool dispatched by the CLIENT must therefore resolve
    // in-process from this agent's instructions, exactly as the agent-turn path
    // does.
    //
    // No dispatcher is attached, so only an in-process resolution can produce
    // a result. A dispatch that reaches the `Source::Toolset` arm fails
    // with "tool dispatch is not configured".
    #[tokio::test]
    async fn client_dispatch_serves_a_runtime_tool_in_process() {
        let router = classify_router();

        let call_id = router
            .dispatch_client_tool("ListInstructions", "{}")
            .await
            .expect("a Runtime-source tool must resolve in-process on the client path");

        let (resp, _) = drain_client_call(&router, &call_id).await;
        assert!(!resp.is_error, "a successful list is not an error");
        let paths: Vec<String> = serde_json::from_str(&proto_common::content_text(&resp.content))
            .expect("the client-visible text is the runtime tool's JSON output");
        assert_eq!(paths, ["skills/classify.md"]);
    }

    // A `Runtime`-source tool that fails must end in a NON-DONE terminal. The
    // client derives its error bit solely from the terminal outcome, so a
    // hardcoded DONE would hand the client an error string as a successful
    // result.
    #[tokio::test]
    async fn client_dispatch_of_a_failing_runtime_tool_ends_in_a_non_done_terminal() {
        use proto_common::tool_result_frame::Frame;

        let router = empty_router();

        let call_id = router
            .dispatch_client_tool("ReadInstructions", r#"{"path":"missing.md"}"#)
            .await
            .expect("a Runtime-source tool must resolve in-process on the client path");

        let (resp, frames) = drain_client_call(&router, &call_id).await;
        let terminal = frames
            .iter()
            .find_map(|f| match f.frame.as_ref() {
                Some(Frame::Complete(c)) => Some(*c),
                _ => None,
            })
            .expect("the synthesized stream ends in a terminal ToolComplete");
        assert_ne!(
            terminal.outcome(),
            proto_common::ToolOutcome::Done,
            "a failed runtime tool must not report a DONE terminal"
        );
        assert!(
            proto_common::content_text(&resp.content).contains("file not found: missing.md"),
            "the runtime tool's error text reaches the client, got {:?}",
            resp.content
        );
        assert!(resp.is_error, "the assembled result carries the error bit");
    }

    /// Drive a dispatched client call to quiescence: its spawned task has
    /// published its terminal frame, or has already retired the session.
    ///
    /// This is a cooperative scheduling point, NOT a sleep. The loop exits on an
    /// OBSERVED terminal state, never on elapsed time. Each yield lets the
    /// runtime task move past one of its await points: the in-process dispatch,
    /// each frame's execution-log append, and the session-map write.
    async fn settle_client_call(router: &ToolRouter, call_id: &str) {
        for _ in 0..10_000 {
            let settled = match router.calls.read().await.get(call_id) {
                // Live session: settled once its buffer holds the terminal.
                Some(handle) => handle.frames.lock().unwrap().iter().any(is_terminal_frame),
                // No session: the task ran to completion and retired it.
                None => true,
            };
            if settled {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("the dispatched call never settled");
    }

    // The client dispatches and awaits as TWO separate RPCs, arriving over two
    // network hops with a real gap between them. A runtime call finishes in
    // microseconds, so by the time the client's `await_tool_result` lands the
    // call is long since complete, and it must still be servable.
    #[tokio::test]
    async fn completed_runtime_call_is_servable_to_a_late_subscriber() {
        let router = classify_router();

        let call_id = router
            .dispatch_client_tool("ListInstructions", "{}")
            .await
            .expect("a Runtime-source tool must resolve in-process on the client path");

        // The gap: the call completes before the client's second RPC arrives.
        settle_client_call(&router, &call_id).await;

        let (resp, _) = drain_client_call(&router, &call_id).await;
        assert!(!resp.is_error, "a successful list is not an error");
        let paths: Vec<String> = serde_json::from_str(&proto_common::content_text(&resp.content))
            .expect("the late subscriber receives the runtime tool's JSON output");
        assert_eq!(paths, ["skills/classify.md"]);
    }

    #[tokio::test]
    async fn client_dispatch_rejects_a_channel_stream_tool_without_calling_the_toolset() {
        let dispatch = in_process_dispatch(&["ts"]);
        let router: ToolRouter = empty_router().with_dispatch(dispatch.clone());

        let err = router
            .dispatch_client_tool("RevealPath", r#"{"path":"/x"}"#)
            .await
            .expect_err("a ChannelStream-source tool is not client-dispatchable");
        assert!(
            err.contains("not client-dispatchable"),
            "the error names the tool as not client-dispatchable, got {err:?}"
        );
        assert_eq!(
            dispatch.pending_call_count().await,
            0,
            "a rejected ChannelStream dispatch must not reach the in-process dispatcher"
        );
    }

    // ---- Tool-call approval ----

    use crate::clients::ServerRequestOutcome;
    use crate::test_doubles::{complete_turn, event_log, EventLog, ScriptedTurns};
    use proto_common::StopReason;

    const APPROVAL: &str = "RequestToolApproval";
    const DENIED: &str = "The user denied this tool call. Do not retry it. Wait for the user.";
    const NO_ANSWER: &str = "The user did not answer in time. This tool call was denied. \
                             Do not retry it. Wait for the user.";
    const UNADVERTISED: &str = "client did not advertise `RequestToolApproval`";
    const RETIRED: &str = "the tool job for Bash was retired before the call reached it";

    /// A router with in-process dispatch, the scripted relay, and toolset
    /// tool `Bash`.
    fn gated_router(dispatch: &Arc<DispatchState>, relay: ScriptedRelay) -> ToolRouter {
        let router: ToolRouter = empty_router()
            .with_dispatch(dispatch.clone())
            .with_relay(relay);
        seed(router, vec![t("Bash")])
    }

    /// `call_tool` from a primary turn, bounded so a call that waits on a tool
    /// pod no test provides fails instead of hanging.
    async fn call_bounded(
        router: &ToolRouter,
        name: &str,
        input_json: &str,
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort> {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            router.call_tool(name, input_json, false, "tc", cancel),
        )
        .await
        .unwrap_or_else(|_| {
            panic!("call_tool({name}) did not return within 5 s: it is waiting on a tool pod")
        })
    }

    fn text_of(resp: &CallToolResponse) -> String {
        proto_common::content_text(&resp.content)
    }

    fn params_of(request: &crate::test_doubles::RelayRequest) -> serde_json::Value {
        serde_json::from_str(&request.params_json).expect("params_json is JSON")
    }

    /// Spawn a pod for `(tool, job_id)` that serves `calls` calls in turn. For
    /// each it logs `pod claimed`, waits 50 ms, logs `pod result`, then feeds
    /// `frames` through. The wait gives anything that should come after the result
    /// time to show up before it when the code under test does not wait.
    fn spawn_logging_pod(
        dispatch: &Arc<DispatchState>,
        tool: &str,
        job_id: &str,
        calls: usize,
        frames: Vec<ToolResultFrame>,
        events: EventLog,
    ) {
        let pod_dispatch = dispatch.clone();
        let tool = tool.to_string();
        let job_id = job_id.to_string();
        tokio::spawn(async move {
            for _ in 0..calls {
                let call = loop {
                    if let Some(call) = pod_dispatch.dequeue_call(&tool, &job_id).await {
                        break call;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                };
                events.lock().unwrap().push("pod claimed".to_string());
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                events.lock().unwrap().push("pod result".to_string());
                let _ = pod_dispatch
                    .forward_result_frames(
                        call.call_id,
                        tokio_stream::iter(
                            frames
                                .clone()
                                .into_iter()
                                .map(Ok::<ToolResultFrame, tonic::Status>),
                        ),
                    )
                    .await;
            }
        });
    }

    fn events_of(events: &EventLog) -> Vec<String> {
        events.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn approved_toolset_call_with_no_warm_pod_is_asked_about_once_before_the_call_is_begun() {
        // No Job is seeded, so no warm pod is attachable and `begin_call`
        // answers with the retired-job error the moment it runs. Getting that
        // error after exactly one approval request means the call was begun,
        // and only after the user approved it.
        let dispatch = in_process_dispatch(&["ts"]);
        let relay = ScriptedRelay::new(vec![RelayAnswer::approve()]);
        let router = gated_router(&dispatch, relay.clone());

        let outcome = call_bounded(&router, "Bash", "{}", &CancellationToken::new()).await;

        assert_eq!(
            relay.requests_for(APPROVAL).len(),
            1,
            "one approval request is sent for the call"
        );
        match outcome {
            Err(DispatchAbort::Error(e)) => {
                assert_eq!(e, RETIRED, "the approved call reaches begin_call")
            }
            other => panic!("expected begin_call's retired-job error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn turn_cancelled_while_a_toolset_call_waits_for_approval_ends_cancelled_without_beginning_the_call(
    ) {
        // No Job is seeded: had `begin_call` run before or without approval,
        // the call would end in the retired-job error, never in Cancelled.
        let dispatch = in_process_dispatch(&["ts"]);
        let relay = ScriptedRelay::new(vec![RelayAnswer::Never]);
        let router = gated_router(&dispatch, relay.clone());
        let cancel = CancellationToken::new();

        let cancel_once_asked = async {
            for _ in 0..400 {
                if !relay.requests_for(APPROVAL).is_empty() {
                    cancel.cancel();
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            cancel.cancel();
            false
        };
        let (outcome, asked) = tokio::join!(
            call_bounded(&router, "Bash", "{}", &cancel),
            cancel_once_asked
        );

        assert!(
            asked,
            "the call waits on an approval request, got {outcome:?}"
        );
        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "a turn cancelled during the approval wait ends cancelled, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn approved_toolset_call_is_asked_about_before_the_warm_pod_claims_it_and_returns_the_pods_result(
    ) {
        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let events = event_log();
        let relay = ScriptedRelay::new(vec![RelayAnswer::approve()]).with_events(events.clone());
        let router = gated_router(&dispatch, relay);
        spawn_logging_pod(
            &dispatch,
            "Bash",
            "job-x",
            1,
            vec![stdout_frame("warm pod output"), done_terminal()],
            events.clone(),
        );

        let resp = call_bounded(&router, "Bash", "{}", &CancellationToken::new())
            .await
            .expect("an approved call returns the tool's result");

        assert_eq!(
            events_of(&events),
            vec![
                format!("request {APPROVAL}"),
                "pod claimed".to_string(),
                "pod result".to_string(),
            ],
            "the approval request comes before the warm pod claims the call"
        );
        assert!(!resp.is_error, "the warm pod's result is not an error");
        assert_eq!(text_of(&resp), "warm pod output");
    }

    #[tokio::test]
    async fn approval_request_names_the_tool_carries_its_input_verbatim_and_asks_for_a_300_second_wait(
    ) {
        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let relay = ScriptedRelay::new(vec![RelayAnswer::deny()]);
        let router = gated_router(&dispatch, relay.clone());
        // Malformed and oddly spaced: only the raw text survives unchanged.
        let raw = "{ \"cmd\" :  \"ls\" ,";

        let _ = call_bounded(&router, "Bash", raw, &CancellationToken::new()).await;

        let requests = relay.requests_for(APPROVAL);
        assert_eq!(requests.len(), 1, "one approval request is sent");
        let params = params_of(&requests[0]);
        assert!(params.is_object(), "params_json is a JSON object: {params}");
        assert_eq!(params["tool"], "Bash");
        assert_eq!(
            params["input"], raw,
            "the input is the model's text, unchanged"
        );
        assert_eq!(requests[0].timeout_seconds, 300);
    }

    #[tokio::test]
    async fn toolset_call_without_an_explicit_approval_is_refused_and_never_handed_to_the_warm_pod()
    {
        fn result(json: &str) -> RelayAnswer {
            RelayAnswer::Outcome(ServerRequestOutcome::Result(json.to_string()))
        }
        // `None` expects the send-and-wait failure on the call's error path.
        let cases: Vec<(&str, RelayAnswer, Option<&str>)> = vec![
            ("denied", RelayAnswer::deny(), Some(DENIED)),
            (
                "client error",
                RelayAnswer::Outcome(ServerRequestOutcome::Error {
                    code: 1,
                    message: "boom".into(),
                }),
                Some(DENIED),
            ),
            (
                "approved as a string",
                result(r#"{"approved":"true"}"#),
                Some(DENIED),
            ),
            (
                "approved as a number",
                result(r#"{"approved":1}"#),
                Some(DENIED),
            ),
            ("not an object", result("true"), Some(DENIED)),
            (
                "timed out",
                RelayAnswer::Outcome(ServerRequestOutcome::TimedOut),
                Some(NO_ANSWER),
            ),
            (
                "method not advertised",
                RelayAnswer::Outcome(ServerRequestOutcome::UnsupportedMethod),
                Some(UNADVERTISED),
            ),
            (
                "no channel stream",
                RelayAnswer::Outcome(ServerRequestOutcome::UnknownChannelStream),
                Some(UNADVERTISED),
            ),
            (
                "send-and-wait failed",
                RelayAnswer::Err("relay unreachable".into()),
                None,
            ),
        ];

        for (case, answer, want) in cases {
            let dispatch = in_process_dispatch(&["ts"]);
            seed_job(&dispatch, "Bash", "job-x").await;
            let relay = ScriptedRelay::new(vec![answer]);
            let router = gated_router(&dispatch, relay.clone());

            let outcome = call_bounded(&router, "Bash", "{}", &CancellationToken::new()).await;

            assert_eq!(
                relay.requests_for(APPROVAL).len(),
                1,
                "{case}: one approval request is sent"
            );
            match (want, outcome) {
                (Some(text), Ok(resp)) => {
                    assert!(resp.is_error, "{case}: the refusal is an error tool result");
                    assert_eq!(text_of(&resp), text, "{case}: the refusal's whole text");
                }
                (None, Err(DispatchAbort::Error(e))) => assert!(
                    e.contains("relay unreachable"),
                    "{case}: the call's error carries the relay error, got {e:?}"
                ),
                (_, other) => panic!("{case}: unexpected outcome {other:?}"),
            }
            assert_eq!(
                dispatch.pending_call_count().await,
                0,
                "{case}: the call is never handed to the warm pod"
            );
        }
    }

    #[tokio::test]
    async fn toolset_call_on_a_router_without_a_relay_is_refused_as_unadvertised_and_never_begun() {
        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let router: ToolRouter = empty_router().with_dispatch(dispatch.clone());
        let router = seed(router, vec![t("Bash")]);

        let resp = call_bounded(&router, "Bash", "{}", &CancellationToken::new())
            .await
            .expect("the refusal is a tool result, not an abort");

        assert!(resp.is_error);
        assert_eq!(text_of(&resp), UNADVERTISED);
        assert_eq!(
            dispatch.pending_call_count().await,
            0,
            "the call is never handed to the warm pod"
        );
    }

    #[tokio::test]
    async fn every_tool_call_asks_for_approval_once_and_runs_once_approved() {
        use crate::dispatch_hub::test_support::{endpoints, silent_endpoint, test_hub};
        let root = tempfile::TempDir::new().unwrap().keep();
        write_md(&root, &format!("{TEST_AG}/skills/a.md"), "# A\n\nAlpha.\n");
        // Nine approvals, plus the answers to the two requests to the user.
        let answers = std::iter::repeat_with(RelayAnswer::approve);
        let relay = ScriptedRelay::new(answers.take(11).collect());
        let hub = test_hub(
            endpoints(vec![], silent_endpoint()),
            ScriptedRelay::default(),
            vec![],
        );
        let log = crate::test_doubles::record("user", "m");
        write_md(&hub.root, &format!("{SUB_PATH}/conversation.log"), &log);
        let router: ToolRouter = ToolRouter::new(
            Arc::new(Instructions::new(root)),
            TEST_AG.to_string(),
            None,
            test_agent_state(),
        )
        .with_relay(relay.clone())
        .with_sub_agents(hub.hub);

        let tools = [
            ("RevealPath", r#"{"path":"/tmp/x"}"#),
            ("RequestUserInput", r#"{"prompt":"which?"}"#),
            ("RequestUserAuth", r#"{"provider":"x"}"#),
            ("ReadInstructions", r#"{"path":"skills/a.md"}"#),
            ("ListInstructions", "{}"),
            ("Think", r#"{"note":"n"}"#),
            ("StartSubAgent", r#"{"sub_agent":"sub","mandate":"m"}"#),
            ("ListSubAgents", "{}"),
            ("ReadSubAgentLog", r#"{"path":"sub/0a1b2c3d"}"#),
        ];
        for (name, input) in tools {
            let resp = router
                .call_tool(name, input, true, "tc", &CancellationToken::new())
                .await
                .unwrap_or_else(|e| panic!("{name} ends in a tool result, got {e:?}"));
            assert!(
                !resp.is_error,
                "{name} works once approved, got {:?}",
                text_of(&resp)
            );
        }
        let asked: Vec<String> = relay
            .requests_for(APPROVAL)
            .iter()
            .map(|r| {
                let params: serde_json::Value = serde_json::from_str(&r.params_json).unwrap();
                params["tool"].as_str().unwrap_or_default().to_string()
            })
            .collect();
        let want: Vec<&str> = tools.iter().map(|(name, _)| *name).collect();
        assert_eq!(asked, want, "each tool call records one approval request");
    }

    #[tokio::test]
    async fn a_cancelled_turn_does_not_wait_for_the_users_answer() {
        let router = empty_router().with_relay(ScriptedRelay::new(vec![
            RelayAnswer::Never,
            RelayAnswer::Never,
        ]));
        let cancel = CancellationToken::new();
        cancel.cancel();

        for (name, input) in [
            ("RequestUserInput", r#"{"prompt":"which?"}"#),
            ("RequestUserAuth", r#"{"provider":"x"}"#),
        ] {
            let outcome = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                router.call_tool(name, input, true, "tc", &cancel),
            )
            .await;
            assert!(
                matches!(outcome, Ok(Err(DispatchAbort::Cancelled))),
                "{name} ends cancelled at once, got {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn client_direct_toolset_call_runs_without_an_approval_request() {
        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let relay = ScriptedRelay::new(vec![]);
        let router = empty_router()
            .with_dispatch(dispatch.clone())
            .with_relay(relay.clone());
        let router = seed(router, vec![t("Bash")]);

        let capture = spawn_pod(&dispatch, "Bash", "job-x", vec![done_terminal()]);
        router
            .dispatch_client_tool("Bash", "{}")
            .await
            .expect("the client's toolset call is dispatched");
        claimed(&capture).await;

        assert!(
            relay.requests_for(APPROVAL).is_empty(),
            "client-direct calls ask for no approval"
        );
    }

    #[test]
    fn catalog_advertises_the_instruction_tools() {
        let names = names(&empty_router());
        for want in ["ReadInstructions", "ListInstructions"] {
            assert!(
                names.iter().any(|n| n == want),
                "{want} missing from {names:?}"
            );
        }
    }

    /// Run one primary turn through `llm_loop` with this router.
    async fn run_primary_turn(
        router: &ToolRouter,
        turns: &mut ScriptedTurns,
    ) -> Result<String, crate::agent::LoopError> {
        use crate::agent::{llm_loop, LoopMode};
        use crate::conversation::AssistantAttribution;
        let log = RwLock::new(crate::test_doubles::throwaway_log());
        let mut sink = crate::turn::NullSink;
        let request = toolset_proto::TurnRequest {
            system: None,
            tools: vec![],
            messages: vec![proto_common::Message {
                role: "user".into(),
                content: vec![text_block("go".into())],
                tool_calls: vec![],
                tool_call_id: None,
                is_error: None,
            }],
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            llm_loop(
                10,
                turns,
                router,
                &log,
                AssistantAttribution::default(),
                request,
                LoopMode {
                    has_client: false,
                    message_id: String::new(),
                    idle_gap: std::time::Duration::from_secs(45),
                    cancel: CancellationToken::new(),
                    inbox: crate::queue::Inbox::default(),
                },
                &mut sink,
            ),
        )
        .await
        .expect("the turn finishes")
    }

    fn two_bash_calls_then_done() -> ScriptedTurns {
        ScriptedTurns::new(vec![
            complete_turn(
                StopReason::ToolUse,
                "",
                &[("tc1", "Bash", "{}"), ("tc2", "Bash", "{}")],
            ),
            complete_turn(StopReason::EndTurn, "done", &[]),
        ])
    }

    #[tokio::test]
    async fn second_toolset_call_in_a_reply_is_asked_about_only_after_the_first_was_answered_and_finished(
    ) {
        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let events = event_log();
        let relay = ScriptedRelay::new(vec![RelayAnswer::approve(), RelayAnswer::approve()])
            .with_events(events.clone());
        let router = gated_router(&dispatch, relay);
        spawn_logging_pod(
            &dispatch,
            "Bash",
            "job-x",
            2,
            vec![done_terminal()],
            events.clone(),
        );

        let mut turns = two_bash_calls_then_done();
        let result = run_primary_turn(&router, &mut turns).await;

        assert!(matches!(result, Ok(ref s) if s == "done"), "got {result:?}");
        let request = format!("request {APPROVAL}");
        assert_eq!(
            events_of(&events),
            vec![
                request.clone(),
                "pod claimed".to_string(),
                "pod result".to_string(),
                request,
                "pod claimed".to_string(),
                "pod result".to_string(),
            ],
            "each call is asked about only after the previous one finished"
        );
    }

    #[tokio::test]
    async fn denying_the_first_toolset_call_in_a_reply_still_asks_about_the_second() {
        let dispatch = in_process_dispatch(&["ts"]);
        seed_job(&dispatch, "Bash", "job-x").await;
        let relay = ScriptedRelay::new(vec![RelayAnswer::deny(), RelayAnswer::approve()]);
        let router = gated_router(&dispatch, relay.clone());
        spawn_pod(&dispatch, "Bash", "job-x", vec![done_terminal()]);

        let mut turns = two_bash_calls_then_done();
        let result = run_primary_turn(&router, &mut turns).await;

        assert!(matches!(result, Ok(ref s) if s == "done"), "got {result:?}");
        assert_eq!(relay.requests_for(APPROVAL).len(), 2);
    }

    const START: &str = "StartSubAgent";

    /// A hub whose attaches dial `endpoint`, with the router that offers it.
    fn start_router(
        endpoint: String,
        relay: ScriptedRelay,
    ) -> (crate::dispatch_hub::test_support::TestHub, ToolRouter) {
        use crate::dispatch_hub::test_support::{endpoints, test_hub};
        let h = test_hub(
            endpoints(vec![], endpoint),
            ScriptedRelay::default(),
            vec![],
        );
        let router = empty_router()
            .with_relay(relay)
            .with_sub_agents(h.hub.clone());
        (h, router)
    }

    #[tokio::test]
    async fn each_start_sub_agent_call_returns_a_new_path_folder_and_job_before_any_attach() {
        use crate::dispatch_hub::test_support::silent_endpoint;
        use crate::job::tests::container_env;
        let relay = ScriptedRelay::new(vec![RelayAnswer::approve(), RelayAnswer::approve()]);
        let (h, router) = start_router(silent_endpoint(), relay);

        let mut paths = Vec::new();
        for mandate in ["first job", "second job"] {
            let input = serde_json::json!({ "sub_agent": "sub", "mandate": mandate });
            let input = input.to_string();
            let cancel = CancellationToken::new();
            let call = router.call_tool(START, &input, false, "tc", &cancel);
            let resp = tokio::time::timeout(std::time::Duration::from_secs(5), call)
                .await
                .expect("the call returns while no sub-agent has attached")
                .expect("a tool result");
            assert!(!resp.is_error, "{}", text_of(&resp));
            paths.push(text_of(&resp));
        }

        for path in &paths {
            let id = path.strip_prefix("sub/").unwrap_or_default();
            assert!(
                id.len() == 8
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "want sub/<8 lowercase hex>, got {path}"
            );
            assert!(h.root.join(path).is_dir(), "no folder for {path}");
        }
        assert_ne!(paths[0], paths[1], "each call gets its own id");
        let jobs = h.jobs.lock().unwrap();
        let envs: Vec<_> = jobs
            .iter()
            .map(|j| {
                (
                    container_env(j, "DISPATCH_PATH"),
                    container_env(j, "MANDATE"),
                )
            })
            .collect();
        assert_eq!(
            envs,
            [
                (Some(paths[0].as_str()), Some("first job")),
                (Some(paths[1].as_str()), Some("second job")),
            ],
            "each call creates one Job with its own path and mandate"
        );
    }

    #[tokio::test]
    async fn a_denied_start_or_a_refused_job_is_an_error_with_no_job_or_folder() {
        use crate::dispatch_hub::test_support::silent_endpoint;
        let relay = ScriptedRelay::new(vec![RelayAnswer::deny(), RelayAnswer::approve()]);
        let (h, router) = start_router(silent_endpoint(), relay);

        // `sub` is denied. `other` is approved, and admission refuses its Job.
        for sub_agent in ["sub", "other"] {
            let input = serde_json::json!({ "sub_agent": sub_agent, "mandate": "m" }).to_string();
            let resp = router
                .call_tool(START, &input, false, "tc", &CancellationToken::new())
                .await
                .expect("a tool result");
            assert!(resp.is_error, "{sub_agent}: {}", text_of(&resp));
            assert!(!h.root.join(sub_agent).exists(), "a folder for {sub_agent}");
        }
        assert!(h.jobs.lock().unwrap().is_empty(), "no Job is admitted");
    }

    #[tokio::test]
    async fn a_turn_that_starts_a_sub_agent_ends_while_the_sub_agent_stays_attached() {
        use crate::clients::RelayRpc;
        use crate::dispatch_hub::test_support::{next_queued, serve, sub_agent_link};
        use crate::job::tests::container_env;
        let link = sub_agent_link();
        let relay = ScriptedRelay::new(vec![RelayAnswer::approve()]);
        let (h, router) = start_router(serve(link.clone()), relay);
        let input = r#"{"sub_agent":"sub","mandate":"m"}"#;
        let mut turns = ScriptedTurns::new(vec![
            complete_turn(StopReason::ToolUse, "", &[("tc1", START, input)]),
            complete_turn(StopReason::EndTurn, "done", &[]),
        ]);

        let result = run_primary_turn(&router, &mut turns).await;
        assert!(matches!(result, Ok(ref s) if s == "done"), "got {result:?}");

        let mut reply = link.clone();
        reply
            .deliver_reply(proto_common::text_content("the answer"))
            .await
            .unwrap();
        let queued = next_queued(&h.agent_state).await;
        let path = h
            .jobs
            .lock()
            .unwrap()
            .first()
            .and_then(|j| container_env(j, "DISPATCH_PATH").map(str::to_string));
        assert_eq!(
            (Some(queued.sender), queued.text),
            (path, "the answer".to_string()),
            "the attach outlives the turn and queues the later answer"
        );
    }

    const SUB_PATH: &str = "sub/0a1b2c3d";
    const SUB_OTHER: &str = "sub/12345678";

    /// A person's call of `name`: its assembled result.
    async fn client_call(router: &ToolRouter, name: &str, input: &str) -> CallToolResponse {
        let call_id = router
            .dispatch_client_tool(name, input)
            .await
            .unwrap_or_else(|e| panic!("a person can call {name}, got {e}"));
        drain_client_call(router, &call_id).await.0
    }

    /// A hub of two fake sub-agents, attached at `SUB_PATH` and `SUB_OTHER`,
    /// and a router over the hub's own agent state that offers it. Returns
    /// each fake's link and inbox.
    async fn two_sub_agents(
        relay: ScriptedRelay,
    ) -> (
        crate::dispatch_hub::test_support::TestHub,
        ToolRouter,
        Vec<(crate::dispatch_link::ParentLink, crate::queue::Inbox)>,
    ) {
        use crate::dispatch_hub::test_support::*;
        let fakes: Vec<_> = (0..2)
            .map(|_| {
                let inbox = crate::queue::Inbox::default();
                (sub_agent_link_with(inbox.clone()), inbox)
            })
            .collect();
        let uris = endpoints(
            vec![
                (SUB_PATH, serve(fakes[0].0.clone())),
                (SUB_OTHER, serve(fakes[1].0.clone())),
            ],
            silent_endpoint(),
        );
        let h = test_hub(uris, ScriptedRelay::default(), vec![]);
        for path in [SUB_PATH, SUB_OTHER] {
            h.hub.attach(path.into());
            attached(&h, path).await;
        }
        let router = ToolRouter::new(
            test_instructions(),
            TEST_AG.to_string(),
            None,
            h.agent_state.clone(),
        )
        .with_relay(relay)
        .with_sub_agents(h.hub.clone());
        (h, router, fakes)
    }

    #[tokio::test]
    async fn a_log_is_read_by_its_path_with_no_live_attach() {
        use crate::dispatch_hub::test_support::silent_endpoint;
        use crate::test_doubles::record;
        let (h, router) = start_router(silent_endpoint(), ScriptedRelay::default());
        let log = record("user", "the mandate") + &record("assistant", "the answer");
        write_md(&h.root, &format!("{SUB_PATH}/conversation.log"), &log);

        let input = serde_json::json!({ "path": SUB_PATH }).to_string();
        let resp = client_call(&router, "ReadSubAgentLog", &input).await;
        let entries: Vec<serde_json::Value> = serde_json::from_str(&text_of(&resp))
            .unwrap_or_else(|_| panic!("want the log's entries, got {}", text_of(&resp)));
        let got: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| {
                (
                    e["role"].as_str().unwrap_or(""),
                    e["text"].as_str().unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(got, [("user", "the mandate"), ("assistant", "the answer")]);
    }

    #[tokio::test]
    async fn only_an_existing_sub_agent_folders_log_is_read() {
        use crate::dispatch_hub::test_support::silent_endpoint;
        let (h, router) = start_router(silent_endpoint(), ScriptedRelay::default());
        let log = crate::test_doubles::record("user", "not yours");
        let mut read = Vec::new();
        for path in [
            "../x",
            "sub/../../conversation.log",
            "sub/ABCDEFGH",
            "conversations",
            "hello-scout/0000abcd",
        ] {
            // Every path but the absent one reaches a log.
            if path != "hello-scout/0000abcd" {
                write_md(&h.root, &format!("{path}/conversation.log"), &log);
            }
            let input = serde_json::json!({ "path": path }).to_string();
            if !client_call(&router, "ReadSubAgentLog", &input)
                .await
                .is_error
            {
                read.push(path);
            }
        }
        assert!(read.is_empty(), "must be refused: {read:?}");
    }

    #[tokio::test]
    async fn a_person_lists_the_ready_paths() {
        use crate::dispatch_hub::test_support::{endpoints, silent_endpoint, test_hub};
        let listed = vec![SUB_PATH.to_string(), SUB_OTHER.to_string()];
        let h = test_hub(
            endpoints(vec![], silent_endpoint()),
            ScriptedRelay::default(),
            listed.clone(),
        );
        let router = empty_router().with_sub_agents(h.hub.clone());

        let resp = client_call(&router, "ListSubAgents", "{}").await;
        let paths: Vec<String> = serde_json::from_str(&text_of(&resp))
            .unwrap_or_else(|_| panic!("want a JSON array of paths, got {}", text_of(&resp)));
        assert_eq!(paths, listed);
    }

    #[tokio::test]
    async fn a_persons_message_reaches_one_sub_agent_unchanged_and_not_the_agent() {
        use crate::dispatch_hub::test_support::landed;
        let (h, router, fakes) = two_sub_agents(ScriptedRelay::default()).await;
        let text = "  Also include the word tulip.\n ";
        let input = serde_json::json!({ "path": SUB_PATH, "message": text }).to_string();
        client_call(&router, "MessageSubAgent", &input).await;

        assert_eq!(landed(&fakes[0].1).await, [text], "it lands unchanged");
        assert!(
            fakes[1].1.drain().is_empty(),
            "the other sub-agent gets none"
        );
        assert!(
            h.agent_state.queue().pop().await.is_none(),
            "nothing is queued for the agent"
        );
        let history = h.agent_state.log().read().await.history_for_provider();
        assert!(history.is_empty(), "the agent's log holds nothing");
    }

    #[tokio::test]
    async fn a_persons_stop_reaches_only_the_named_sub_agent() {
        let (_h, router, fakes) = two_sub_agents(ScriptedRelay::default()).await;
        let input = serde_json::json!({ "path": SUB_PATH }).to_string();
        client_call(&router, "StopSubAgent", &input).await;

        let wait = |ms| std::time::Duration::from_millis(ms);
        tokio::time::timeout(wait(5000), fakes[0].0.stopped())
            .await
            .expect("the named sub-agent stops");
        let other = tokio::time::timeout(wait(200), fakes[1].0.stopped()).await;
        assert!(other.is_err(), "the other sub-agent runs on");
    }

    #[tokio::test]
    async fn the_agents_message_and_stop_succeed_and_only_they_skip_approval() {
        // An empty script refuses any approval request.
        let relay = ScriptedRelay::default();
        let (_h, router, _) = two_sub_agents(relay.clone()).await;
        let message = serde_json::json!({ "path": SUB_PATH, "message": "tulip" }).to_string();
        let stop = serde_json::json!({ "path": SUB_PATH }).to_string();
        let cancel = CancellationToken::new();
        for (name, input) in [("MessageSubAgent", message), ("StopSubAgent", stop)] {
            let resp = router.call_tool(name, &input, false, "tc", &cancel).await;
            assert!(
                !resp.expect("a tool result").is_error,
                "{name} reaches the hub"
            );
        }
        assert!(
            relay.requests_for(APPROVAL).is_empty(),
            "the agent's message and stop ask no approval"
        );

        // With no hub, a toolset tool of the same name still asks.
        let relay = ScriptedRelay::new(vec![RelayAnswer::deny()]);
        let router = seed(
            empty_router().with_relay(relay.clone()),
            vec![t("StopSubAgent")],
        );
        let cancel = CancellationToken::new();
        let resp = router.call_tool("StopSubAgent", "{}", false, "tc", &cancel);
        resp.await.expect("a tool result");
        assert_eq!(
            relay.requests_for(APPROVAL).len(),
            1,
            "one approval request"
        );
    }

    #[tokio::test]
    async fn a_person_cannot_start_a_sub_agent() {
        use crate::dispatch_hub::test_support::silent_endpoint;
        let (h, router) = start_router(silent_endpoint(), ScriptedRelay::default());
        let resp = client_call(&router, START, r#"{"sub_agent":"sub","mandate":"m"}"#).await;
        assert!(resp.is_error, "want an error frame, got {}", text_of(&resp));
        assert!(h.jobs.lock().unwrap().is_empty(), "no Job is created");
    }
}
