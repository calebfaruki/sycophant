//! Tool router: fan-in across toolset-ctrl and the harness-local runtime
//! (Agent / Agents / Skill / Skills).
//!
//! Every tool the LLM sees has a `Source`. `Toolset` tools advertise
//! themselves via a gRPC stream from toolset-ctrl and dispatch via gRPC.
//! `Runtime` tools (`Agent`, `Agents`, `Skill`, `Skills`, `Think`,
//! `RecentTurns`) are statically defined here and dispatched in-process —
//! persona and skill content is read directly from this workspace's mounted
//! kernel volume; `Agent` also composes a toolset `Turn`. They never fabricate
//! results.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use proto_common::{CallToolResponse, ToolDefinition, ToolInfo, ToolResultFrame};
use tokio::sync::{broadcast, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::Status;

use tokio_util::sync::CancellationToken;

use crate::channel_tools;
use crate::clients::{RelayClient, RelayRpc, ToolsetClient, ToolsetRpc};
use crate::execution_log::{assemble_from_frames, frames_from_response, ExecutionLogWriter};
use crate::kernel::Kernel;
use crate::registry::ConversationRegistry;
use crate::runtime_tools::{self, DispatchAbort};

/// Which subsystem owns a given tool name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Toolset,
    Runtime,
    /// Client-side tool. Executes on the user's device (Flutter app
    /// today) via a `ServerRequest` over the channel. Dispatch routes
    /// through the relay gateway's `SendServerNotification` /
    /// `SendServerRequestAndAwait` depending on the tool's `Kind`.
    Channel,
}

/// Tool dispatch surface used by the agent loop. The trait carries the
/// runtime context (toolset, conversation id) so `Runtime`-source
/// tools can compose controller calls without the router having to own
/// its own toolset handle. `tool_definitions` lives on the concrete
/// `ToolRouter` instead — the loop reads it directly via the snapshot.
#[async_trait::async_trait]
pub(crate) trait ToolDispatcher: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        toolset: &mut dyn ToolsetRpc,
        conversation_id: &str,
        reply_channel: Option<&str>,
        tool_call_id: &str,
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort>;
}

pub(crate) struct ToolRouter<A = ToolsetClient> {
    /// This workspace's kernel reader, backing the in-process `Runtime` arm
    /// (`Agent`/`Agents` personas, `Skill`/`Skills` content). Reads the mounted
    /// read-only kernel volume; no network hop.
    kernel: Arc<Kernel>,
    /// This harness's own workspace name. Each harness serves only its own
    /// workspace's kernel; the name roots every kernel read.
    workspace: String,
    /// Generic over the toolset RPC surface (the fake seam) so tests
    /// back the `Source::Toolset` arm with a `FakeToolset`. Production uses
    /// `ToolsetClient`, selected by the default type parameter.
    toolset: Option<A>,
    /// Dialer for the relay gateway's internal listener. `Channel`-source
    /// tools push `ServerRequest` frames through it. `None` in tests and when
    /// no gateway is configured.
    relay: Option<RelayClient>,
    /// Conversation registry — `Runtime`-source tools reach it for
    /// minting sub-conversations (`Agent`) and reading history
    /// (`RecentTurns`).
    registry: Arc<ConversationRegistry>,
    /// Live snapshot keyed by tool name. Toolset pushes overwrite their own
    /// entries; runtime tools are inserted at construction time and never
    /// change. Lock-free reads via `ArcSwap`; writers serialize through
    /// `apply_lock`.
    tools: ArcSwap<Vec<(ToolInfo, Source)>>,
    /// Serializes the two `apply_*_tools` watcher tasks so concurrent
    /// read-modify-swap can't drop one source's update. No `.await`
    /// crosses the guard, so `std::sync::Mutex` is correct.
    apply_lock: std::sync::Mutex<()>,
    /// Explicit execution-log override. When set, every call persists here
    /// regardless of conversation — a test seam. When unset (production), the
    /// per-conversation writer is derived from the registry
    /// (`registry.execution_log_for`), so a call's frames land in its own
    /// conversation's `execution.json`.
    execution_log: Option<Arc<dyn ExecutionLogWriter>>,
    /// Live client-facing tool-call sessions, keyed by toolset `call_id`. Minted
    /// on `dispatch_client_tool`, retired when the runtime's terminal arrives.
    /// A present entry means the call is in flight (cancelable); an absent one
    /// is finished (served from the execution log) or never dispatched.
    calls: Arc<RwLock<HashMap<String, CallSessionHandle>>>,
}

/// One live dispatch/await/cancel session's shared state. The single toolset
/// stream consumer appends each frame to the execution log AND publishes it to
/// `sender` (live fan-out) while recording it in `frames` (replay-so-far). A
/// late `await` subscriber snapshots `frames` and subscribes to `sender` under
/// the same lock so no frame slips between the snapshot and the subscription.
#[derive(Clone)]
struct CallSessionHandle {
    sender: broadcast::Sender<ToolResultFrame>,
    frames: Arc<std::sync::Mutex<Vec<ToolResultFrame>>>,
}

/// How long a finished in-process runtime call stays servable from its live
/// session after the dispatch task completes.
///
/// A runtime call resolves in microseconds but the client awaits it on a
/// separate RPC, and a call with no conversation named has no durable record to
/// fall back on. This window has to outlast the client's dispatch-to-await round
/// trip, and it bounds how long a finished session occupies `calls` so the map
/// cannot grow without bound. Sixty seconds clears the round trip by orders of
/// magnitude.
const RUNTIME_CALL_RETENTION: std::time::Duration = std::time::Duration::from_secs(60);

/// Stand-in for the toolset seam on the client-driven runtime path when no
/// toolset client is configured. Only the `Agent` arm of
/// `runtime_tools::dispatch` reaches for it, so every kernel-only runtime tool
/// stays correct without one; an `Agent` call that does reach for it gets a
/// named error rather than a silently different answer.
struct UnconfiguredToolset;

#[async_trait::async_trait]
impl ToolsetRpc for UnconfiguredToolset {
    async fn turn(
        &mut self,
        _request: toolset_proto::TurnRequest,
    ) -> Result<Box<dyn crate::clients::TurnSource>, String> {
        Err("toolset client not configured".into())
    }
    async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
        Err("toolset client not configured".into())
    }
    async fn watch_tools(
        &mut self,
    ) -> Result<tonic::Streaming<proto_common::ToolListUpdate>, String> {
        Err("toolset client not configured".into())
    }
    async fn begin_tool_call(&mut self, _name: &str, _input_json: &str) -> Result<String, String> {
        Err("toolset client not configured".into())
    }
    async fn await_tool_result(
        &mut self,
        _call_id: &str,
    ) -> Result<Box<dyn crate::clients::ToolResultStream>, String> {
        Err("toolset client not configured".into())
    }
    async fn cancel_tool_call(&mut self, _call_id: &str) -> Result<bool, String> {
        Err("toolset client not configured".into())
    }
}

/// Whether a frame is the terminal `ToolComplete`.
fn is_terminal_frame(frame: &ToolResultFrame) -> bool {
    matches!(
        frame.frame,
        Some(proto_common::tool_result_frame::Frame::Complete(_))
    )
}

/// A synthetic FAILED terminal. Closes a client's await stream when the call's
/// own frame stream ended with no terminal — the live session retired on a
/// stream error, or a persisted record was truncated. Every client-driven call
/// must end in one of the three tool outcomes.
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

impl<A: ToolsetRpc + Clone + Send + 'static> ToolRouter<A> {
    pub(crate) fn new(
        kernel: Arc<Kernel>,
        workspace: String,
        toolset: Option<A>,
        relay: Option<RelayClient>,
        registry: Arc<ConversationRegistry>,
    ) -> Self {
        let mut tools: Vec<(ToolInfo, Source)> = runtime_tools::tool_definitions()
            .into_iter()
            .map(|t| (t, Source::Runtime))
            .collect();
        // Channel-source tools (RevealPath, RequestUserInput, RequestUserAuth)
        // are framework-defined just like runtime tools — declared in
        // `channel_tools::tool_definitions` and inserted at construction.
        tools.extend(
            channel_tools::tool_definitions()
                .into_iter()
                .map(|t| (t, Source::Channel)),
        );
        // Ensure runtime + channel names don't collide among themselves.
        let mut seen = std::collections::HashSet::new();
        for (info, _) in &tools {
            assert!(
                seen.insert(info.name.clone()),
                "duplicate framework tool: {}",
                info.name
            );
        }
        // Sort for deterministic advertisement order.
        tools.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        Self {
            kernel,
            workspace,
            toolset,
            relay,
            registry,
            tools: ArcSwap::new(Arc::new(tools)),
            apply_lock: std::sync::Mutex::new(()),
            execution_log: None,
            calls: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Resolve the execution-log writer for a call in `conversation_id`: the
    /// explicit override when one is set (a test seam), otherwise the
    /// conversation's own writer derived from the registry. `None` when no
    /// override is set and the backend has no local directory.
    async fn execution_log_for(
        &self,
        conversation_id: &str,
    ) -> Option<Arc<dyn ExecutionLogWriter>> {
        if let Some(writer) = &self.execution_log {
            return Some(writer.clone());
        }
        self.registry.execution_log_for(conversation_id).await
    }

    /// Override the per-conversation execution-log derivation with a single
    /// explicit writer, so a test can point every call at one store it inspects
    /// directly. Production derives each conversation's writer from the
    /// registry and never calls this.
    #[cfg(test)]
    pub(crate) fn with_execution_log(mut self, writer: Arc<dyn ExecutionLogWriter>) -> Self {
        self.execution_log = Some(writer);
        self
    }

    /// Replace the toolset-owned subset of the tool list with a fresh
    /// snapshot. Runtime entries are preserved. Errors hard on any name
    /// collision with an existing source.
    pub(crate) fn apply_toolset_tools(&self, tools: Vec<ToolInfo>) -> Result<(), String> {
        self.apply_source(Source::Toolset, tools)
    }

    fn apply_source(&self, source: Source, snapshot: Vec<ToolInfo>) -> Result<(), String> {
        // Serialize concurrent watcher RMWs. The collision check below
        // reads the live snapshot — both halves must run inside the
        // writer-lock scope or two simultaneous applies of colliding
        // names could both pass it.
        let _guard = self
            .apply_lock
            .lock()
            .expect("apply_lock poisoned — unrecoverable");
        let current = self.tools.load();
        // Detect collisions against entries owned by a different source.
        // Runtime ones are framework-defined; toolset ones are
        // operator-configured. Either side colliding with another is a
        // configuration bug we want to surface loudly.
        for tool in &snapshot {
            for (existing_tool, existing_source) in current.iter() {
                if existing_tool.name == tool.name && *existing_source != source {
                    return Err(format!(
                        "tool name collision: {} advertised by both {:?} and {:?}",
                        tool.name, existing_source, source
                    ));
                }
            }
        }
        let mut next: Vec<(ToolInfo, Source)> = current.iter().cloned().collect();
        next.retain(|(_, s)| *s != source);
        next.extend(snapshot.into_iter().map(|t| (t, source)));
        next.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        let len = next.len();
        self.tools.store(Arc::new(next));
        tracing::info!(count = len, source = ?source, "tool router refreshed");
        Ok(())
    }

    pub(crate) fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .load()
            .iter()
            .map(|(t, _)| ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters_json: t.parameters_json.clone(),
            })
            .collect()
    }

    fn source_of(&self, name: &str) -> Option<Source> {
        self.tools
            .load()
            .iter()
            .find(|(t, _)| t.name == name)
            .map(|(_, s)| *s)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        toolset: &mut dyn ToolsetRpc,
        conversation_id: &str,
        reply_channel: Option<&str>,
        tool_call_id: &str,
        // The parent turn's cancel token. The `Toolset` arm races it against
        // the frame-stream consume; the `Runtime` arm forwards it into
        // sub-agent dispatch. The `Channel` arm ignores it — its unary
        // dispatch has no in-flight point to interrupt.
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort> {
        let source = self
            .source_of(name)
            .ok_or_else(|| DispatchAbort::Error(format!("unknown tool: {name}")))?;
        match source {
            Source::Toolset => {
                let mut client = self
                    .toolset
                    .clone()
                    .ok_or_else(|| DispatchAbort::Error("toolset client not configured".into()))?;
                // Learn the call_id before the result exists, then race the
                // frame-stream consume against the turn's cancel token. Biased so
                // an already-fired token is observed before the first poll.
                let call_id = client
                    .begin_tool_call(name, input_json)
                    .await
                    .map_err(DispatchAbort::Error)?;
                let exec = self.execution_log_for(conversation_id).await;
                // A second handle and owned ids for the detached cancel+drain
                // task the cancel branch spawns.
                let mut drain_client = client.clone();
                let drain_id = call_id.clone();
                let drain_exec = exec.clone();

                // Consume the ordered frame stream to its terminal `ToolComplete`,
                // appending each frame to the execution log as it arrives so the
                // agent-turn call is re-subscribable from the same `.frames`
                // record the client path writes. Append failures are non-fatal.
                let consume = async {
                    let mut stream = client
                        .await_tool_result(&call_id)
                        .await
                        .map_err(DispatchAbort::Error)?;
                    let mut frames = Vec::new();
                    while let Some(item) = stream.next_frame().await {
                        let frame = item.map_err(DispatchAbort::Error)?;
                        let terminal = is_terminal_frame(&frame);
                        if let Some(writer) = &exec {
                            if let Err(e) = writer.append_frame(&call_id, &frame).await {
                                tracing::warn!(error = %e, call_id = %call_id, "failed to append execution frame");
                            }
                        }
                        frames.push(frame);
                        if terminal {
                            break;
                        }
                    }
                    Ok::<_, DispatchAbort>(frames)
                };

                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        // Detached, so the turn is not blocked: it returns
                        // `Cancelled` at once, never folded into an `Ok(is_error)`.
                        // The spawned task fires exactly one `cancel_tool_call`,
                        // then DRAINS the frame stream to its terminal, appending
                        // each frame to the execution log — so the runtime's own
                        // terminal (outcome Canceled) lands rather than being
                        // dropped with the abandoned consume future. Append
                        // failures are non-fatal.
                        tokio::spawn(async move {
                            let _ = drain_client.cancel_tool_call(&drain_id).await;
                            if let Ok(mut stream) = drain_client.await_tool_result(&drain_id).await {
                                while let Some(item) = stream.next_frame().await {
                                    let frame = match item {
                                        Ok(f) => f,
                                        Err(e) => {
                                            tracing::warn!(error = %e, call_id = %drain_id, "drain frame stream error");
                                            break;
                                        }
                                    };
                                    let terminal = is_terminal_frame(&frame);
                                    if let Some(writer) = &drain_exec {
                                        if let Err(e) = writer.append_frame(&drain_id, &frame).await {
                                            tracing::warn!(error = %e, call_id = %drain_id, "failed to append execution frame");
                                        }
                                    }
                                    if terminal {
                                        break;
                                    }
                                }
                            }
                        });
                        Err(DispatchAbort::Cancelled)
                    }
                    frames = consume => {
                        let frames = frames?;
                        // Assemble the model-facing result from the same in-memory
                        // buffer whose frames were appended to the execution log
                        // above — one store format, no separate end-of-call write.
                        Ok(assemble_from_frames(&frames))
                    }
                }
            }
            Source::Runtime => {
                let mut gateway = self.relay.clone();
                runtime_tools::dispatch(
                    name,
                    input_json,
                    &self.kernel,
                    &self.workspace,
                    toolset,
                    &self.registry,
                    conversation_id,
                    reply_channel,
                    gateway.as_mut().map(|g| g as &mut dyn RelayRpc),
                    cancel,
                )
                .await
            }
            Source::Channel => {
                let mut gateway = self.relay.clone().ok_or_else(|| {
                    DispatchAbort::Error("relay gateway client not configured".into())
                })?;
                channel_tools::dispatch(name, input_json, &mut gateway, reply_channel, tool_call_id)
                    .await
                    .map_err(DispatchAbort::Error)
            }
        }
    }

    /// Client-facing dispatch. The harness is the authority for every tool: it
    /// decides what exists and what runs, and the only thing a tool's `Source`
    /// varies is WHERE the work happens. So this resolves the source first, the
    /// same way the agent-turn `call_tool` does, and only then chooses between
    /// delegating to the toolset controller for sandboxed execution and running
    /// the tool in-process. Either way the caller gets a `call_id` back before
    /// the call resolves, and awaits its frames on `await_client_tool`.
    ///
    /// An unknown name is answered here rather than after a controller
    /// round-trip.
    pub(crate) async fn dispatch_client_tool(
        &self,
        name: &str,
        input_json: &str,
        conversation_id: &str,
    ) -> Result<String, String> {
        match self
            .source_of(name)
            .ok_or_else(|| format!("unknown tool: {name}"))?
        {
            Source::Toolset => {
                self.dispatch_client_toolset_tool(name, input_json, conversation_id)
                    .await
            }
            Source::Runtime => {
                self.dispatch_client_runtime_tool(name, input_json, conversation_id)
                    .await
            }
            // A channel tool executes on the user's own device via a
            // `ServerRequest`. Dispatching one FROM that device would be a
            // request looping back at its own sender, so it is refused by name
            // instead of falling through to a controller that would answer with
            // a confusing `unknown tool`.
            Source::Channel => Err(format!(
                "{name} is not client-dispatchable: channel tools execute on the client itself"
            )),
        }
    }

    /// Delegate to toolset-ctrl for sandboxed execution: mint the toolset
    /// `call_id`, register a live session, spawn its single stream consumer, and
    /// return the id before the call resolves. The consumer is the one owner of
    /// the toolset frame stream: it appends every frame to the execution log and
    /// publishes it to the session's fan-out, staying subscribed through a
    /// cancel — only the runtime's terminal ends it — then retires the session.
    async fn dispatch_client_toolset_tool(
        &self,
        name: &str,
        input_json: &str,
        conversation_id: &str,
    ) -> Result<String, String> {
        let mut client = self
            .toolset
            .clone()
            .ok_or_else(|| "toolset client not configured".to_string())?;
        let call_id = client.begin_tool_call(name, input_json).await?;

        let (sender, _) = broadcast::channel(256);
        let frames = Arc::new(std::sync::Mutex::new(Vec::new()));
        self.calls.write().await.insert(
            call_id.clone(),
            CallSessionHandle {
                sender: sender.clone(),
                frames: frames.clone(),
            },
        );

        let exec = self.execution_log_for(conversation_id).await;
        let calls = self.calls.clone();
        let cid = call_id.clone();
        tokio::spawn(async move {
            match client.await_tool_result(&cid).await {
                Ok(mut stream) => {
                    while let Some(item) = stream.next_frame().await {
                        let frame = match item {
                            Ok(f) => f,
                            Err(e) => {
                                tracing::warn!(error = %e, call_id = %cid, "session frame stream error");
                                break;
                            }
                        };
                        let terminal = is_terminal_frame(&frame);
                        if let Some(writer) = &exec {
                            if let Err(e) = writer.append_frame(&cid, &frame).await {
                                tracing::warn!(error = %e, call_id = %cid, "failed to append execution frame");
                            }
                        }
                        // Push then broadcast as ONE critical section: a late
                        // subscriber that takes this lock either sees the frame in
                        // its snapshot (before push) or on the fan-out (after
                        // send), never both. `broadcast::Sender::send` is
                        // synchronous and non-blocking, so no `.await` crosses the
                        // guard.
                        {
                            let mut guard = frames.lock().unwrap();
                            guard.push(frame.clone());
                            let _ = sender.send(frame);
                        }
                        if terminal {
                            break;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, call_id = %cid, "session await_tool_result failed");
                }
            }
            // Retire the session: a re-subscriber is served from the persisted
            // execution log, not this live handle.
            calls.write().await.remove(&cid);
        });

        Ok(call_id)
    }

    /// Run a `Runtime`-source tool in-process for the client, giving it the same
    /// call_id / frame-stream / execution-log contract a delegated call has.
    ///
    /// The dispatch runs in a spawned task rather than inline so the caller gets
    /// its id immediately and the call stays servable from the live session even
    /// when no conversation is named and nothing persists. The id is minted here
    /// — no controller is involved — and the `rt-` prefix marks that provenance
    /// in the conversation's `execution.json`.
    async fn dispatch_client_runtime_tool(
        &self,
        name: &str,
        input_json: &str,
        conversation_id: &str,
    ) -> Result<String, String> {
        let call_id = format!("rt-{}", uuid::Uuid::new_v4());

        let (sender, _) = broadcast::channel(256);
        let frames = Arc::new(std::sync::Mutex::new(Vec::new()));
        self.calls.write().await.insert(
            call_id.clone(),
            CallSessionHandle {
                sender: sender.clone(),
                frames: frames.clone(),
            },
        );

        let exec = self.execution_log_for(conversation_id).await;
        let calls = self.calls.clone();
        let cid = call_id.clone();
        // Owned inputs for the detached task.
        let kernel = self.kernel.clone();
        let workspace = self.workspace.clone();
        let registry = self.registry.clone();
        let conversation_id = conversation_id.to_string();
        let name = name.to_string();
        let input_json = input_json.to_string();
        let mut toolset = self.toolset.clone();
        let mut relay = self.relay.clone();

        tokio::spawn(async move {
            let mut unconfigured = UnconfiguredToolset;
            let turn_seam: &mut dyn ToolsetRpc = match toolset.as_mut() {
                Some(client) => client,
                None => &mut unconfigured,
            };
            // A client-driven runtime call has no turn to be cancelled with, so
            // this token is never fired.
            let cancel = CancellationToken::new();
            let dispatched = runtime_tools::dispatch(
                &name,
                &input_json,
                &kernel,
                &workspace,
                turn_seam,
                &registry,
                &conversation_id,
                None,
                relay.as_mut().map(|g| g as &mut dyn RelayRpc),
                &cancel,
            )
            .await;

            // Both aborts become an error result the client can render: it
            // subscribed to a frame stream and must get a terminal on it either
            // way. `Cancelled` is unreachable through the never-fired token
            // above, and is carried rather than dropped if that ever changes.
            let response = match dispatched {
                Ok(response) => response,
                Err(DispatchAbort::Error(e)) => CallToolResponse {
                    content: vec![crate::agent::text_block(format!("{name} error: {e}"))],
                    is_error: true,
                },
                Err(DispatchAbort::Cancelled) => CallToolResponse {
                    content: vec![crate::agent::text_block(format!("{name} was cancelled"))],
                    is_error: true,
                },
            };

            for frame in frames_from_response(&response) {
                if let Some(writer) = &exec {
                    if let Err(e) = writer.append_frame(&cid, &frame).await {
                        tracing::warn!(error = %e, call_id = %cid, "failed to append execution frame");
                    }
                }
                // Push then broadcast as ONE critical section, on the same terms
                // as the toolset consumer: a late subscriber taking this lock
                // sees the frame in its snapshot or on the fan-out, never both.
                {
                    let mut guard = frames.lock().unwrap();
                    guard.push(frame.clone());
                    let _ = sender.send(frame);
                }
            }
            tokio::time::sleep(RUNTIME_CALL_RETENTION).await;
            calls.write().await.remove(&cid);
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
        conversation_id: &str,
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
                tokio::spawn(async move {
                    // Replay the frames seen so far; a terminal among them ends
                    // the stream here.
                    let mut terminal_seen = false;
                    for frame in snapshot {
                        let terminal = is_terminal_frame(&frame);
                        if tx.send(Ok(frame)).await.is_err() {
                            return;
                        }
                        if terminal {
                            terminal_seen = true;
                            break;
                        }
                    }
                    // Otherwise follow the live fan-out to the terminal. A recv
                    // error (lagged, or the sender dropped without a terminal
                    // because the consumer retired on a stream error) ends the
                    // while-let.
                    if !terminal_seen {
                        while let Ok(frame) = sub.recv().await {
                            let terminal = is_terminal_frame(&frame);
                            if tx.send(Ok(frame)).await.is_err() {
                                return;
                            }
                            if terminal {
                                terminal_seen = true;
                                break;
                            }
                        }
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
                // from the durable execution log. An override writer (a test
                // seam) serves every conversation and short-circuits resolution;
                // otherwise open exactly the owning conversation's execution.json
                // via the registry-derived writer and read the call by id. The
                // writer is derived from the on-disk conversation dir, not an
                // in-memory table, so resolution survives a harness restart.
                // An empty (or unknown) conversation_id yields no writer -> None,
                // and a conversation-less call persists nothing to find anyway.
                let persisted = match &self.execution_log {
                    Some(writer) => writer.read(call_id).await,
                    None => match self.registry.execution_log_for(conversation_id).await {
                        Some(writer) => writer.read(call_id).await,
                        None => None,
                    },
                };
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

    /// Client-facing cancel: forward to the toolset only when the call is still
    /// in flight, and return whether it was canceled. An unknown or
    /// already-retired call_id is answered here — no cancel is forwarded — and
    /// reports that no call was canceled.
    pub(crate) async fn cancel_client_tool(&self, call_id: &str) -> bool {
        if !self.calls.read().await.contains_key(call_id) {
            return false;
        }
        let mut client = match self.toolset.clone() {
            Some(c) => c,
            None => return false,
        };
        client.cancel_tool_call(call_id).await.unwrap_or(false)
    }
}

#[async_trait::async_trait]
impl<A: ToolsetRpc + Clone + Send + Sync + 'static> ToolDispatcher for ToolRouter<A> {
    async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        toolset: &mut dyn ToolsetRpc,
        conversation_id: &str,
        reply_channel: Option<&str>,
        tool_call_id: &str,
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort> {
        ToolRouter::call_tool(
            self,
            name,
            input_json,
            toolset,
            conversation_id,
            reply_channel,
            tool_call_id,
            cancel,
        )
        .await
    }
}

/// Background task: hold a `WatchTools` stream open against `client` and feed
/// each pushed snapshot to `apply` (the router's per-source setter). Reconnects
/// with backoff so a transient error or controller restart doesn't permanently
/// detach the workspace from tool updates. `component` labels the logs. Backed
/// by the `ToolsetRpc` seam so tests can drive it with a fake.
async fn watch_tools_loop<C: ToolsetRpc>(
    mut client: C,
    router: Arc<ToolRouter>,
    apply: fn(&ToolRouter, Vec<ToolInfo>) -> Result<(), String>,
    component: &'static str,
    mut initial_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    loop {
        match client.watch_tools().await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(update) => {
                            if let Err(e) = apply(&router, update.tools) {
                                tracing::error!(error = %e, component, "tool snapshot rejected");
                            }
                            if let Some(tx) = initial_tx.take() {
                                let _ = tx.send(());
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, component, "watch_tools stream error, reconnecting");
                            break;
                        }
                    }
                }
                tracing::info!(component, "watch_tools stream closed, reconnecting");
            }
            Err(e) => {
                tracing::warn!(error = %e, component, "watch_tools subscribe failed, retrying");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

pub(crate) async fn watch_toolset_tools(
    client: ToolsetClient,
    router: Arc<ToolRouter>,
    initial_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    watch_tools_loop(
        client,
        router,
        ToolRouter::apply_toolset_tools,
        "toolset",
        initial_tx,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::Kernel;

    const WS: &str = "ws";

    /// An empty-workspace kernel over a throwaway temp dir. The dir is leaked
    /// (never cleaned) so the returned `Arc<Kernel>` can outlive this call.
    fn test_kernel() -> Arc<Kernel> {
        let root = tempfile::TempDir::new().unwrap().keep();
        std::fs::create_dir_all(root.join(WS)).unwrap();
        Arc::new(Kernel::new(root))
    }

    fn t(name: &str) -> ToolInfo {
        ToolInfo {
            name: name.into(),
            description: format!("desc:{name}"),
            parameters_json: "{}".into(),
        }
    }

    fn test_registry() -> Arc<ConversationRegistry> {
        use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
        let root = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(root));
        Arc::new(ConversationRegistry::new(factory))
    }

    fn empty_router() -> ToolRouter {
        ToolRouter::new(test_kernel(), WS.to_string(), None, None, test_registry())
    }

    fn names(router: &ToolRouter) -> Vec<String> {
        router
            .tool_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    #[test]
    fn new_router_advertises_runtime_tools() {
        let router = empty_router();
        let names = names(&router);
        assert!(names.iter().any(|n| n == "Agent"));
        assert!(names.iter().any(|n| n == "Agents"));
    }

    #[test]
    fn apply_toolset_tools_preserves_runtime_entries() {
        let router = empty_router();
        router
            .apply_toolset_tools(vec![t("Bash"), t("Git")])
            .unwrap();
        let names = names(&router);
        assert!(names.iter().any(|n| n == "Agent"));
        assert!(names.iter().any(|n| n == "Bash"));
        assert!(names.iter().any(|n| n == "Git"));
    }

    #[test]
    fn apply_replaces_within_same_source() {
        let router = empty_router();
        router.apply_toolset_tools(vec![t("Bash")]).unwrap();
        router.apply_toolset_tools(vec![t("Git")]).unwrap();
        let names = names(&router);
        assert!(!names.iter().any(|n| n == "Bash"));
        assert!(names.iter().any(|n| n == "Git"));
    }

    #[test]
    fn apply_rejects_collision_with_runtime_tool() {
        let router = empty_router();
        // `Agent` and `Skill` are built-in runtime tools; an toolset snapshot
        // colliding with either is a configuration bug the router rejects.
        assert!(router
            .apply_toolset_tools(vec![t("Agent")])
            .unwrap_err()
            .contains("collision"));
        assert!(router
            .apply_toolset_tools(vec![t("Skill")])
            .unwrap_err()
            .contains("collision"));
    }

    /// Assert a dispatch error carries `needle`. The router now returns
    /// `DispatchAbort`; routing-attribution errors surface as `Error(..)`.
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
    async fn call_tool_unknown_name_rejected() {
        let router = empty_router();
        let mut tb = UnconfiguredToolset;
        let cancel = CancellationToken::new();
        let err = router
            .call_tool("Nope", "{}", &mut tb, "conv", None, "tc", &cancel)
            .await
            .unwrap_err();
        assert_dispatch_error(err, "unknown tool");
    }

    #[tokio::test]
    async fn call_tool_routes_toolset_through_toolset_client() {
        let router = empty_router();
        router.apply_toolset_tools(vec![t("Bash")]).unwrap();
        let mut tb = UnconfiguredToolset;
        let cancel = CancellationToken::new();
        let err = router
            .call_tool("Bash", "{}", &mut tb, "conv", None, "tc", &cancel)
            .await
            .unwrap_err();
        // No toolset client wired in this test; the routing decision
        // proves the source attribution worked.
        assert_dispatch_error(err, "toolset client not configured");
    }

    #[tokio::test]
    async fn call_tool_routes_skill_through_in_process_runtime_dispatch() {
        // `Skill`/`Skills` are now Runtime-source, served in-process from the
        // kernel — no gRPC hop. Against an empty kernel, `Skills` returns an
        // empty list (not an "unknown tool" or "not configured" error),
        // proving the call reached the in-process kernel reader.
        let router = empty_router();
        let mut tb = UnconfiguredToolset;
        let cancel = CancellationToken::new();
        let resp = router
            .call_tool("Skills", "{}", &mut tb, "conv", None, "tc", &cancel)
            .await
            .expect("Skills routes to the in-process runtime dispatch");
        assert!(!resp.is_error);
        assert_eq!(crate::agent::collect_text(&resp.content), "[]");
    }

    #[tokio::test]
    async fn call_tool_routes_runtime_through_runtime_dispatch() {
        let router = empty_router();
        let mut tb = UnconfiguredToolset;
        let cancel = CancellationToken::new();
        // `Agents` on an empty kernel returns an empty list in-process,
        // proving Runtime source attribution routed to the kernel reader
        // (an unrouted call would hit the "unknown tool" branch instead).
        let resp = router
            .call_tool("Agents", "{}", &mut tb, "conv", None, "tc", &cancel)
            .await
            .expect("Agents routes to the in-process runtime dispatch");
        assert!(!resp.is_error);
        assert_eq!(crate::agent::collect_text(&resp.content), "[]");
    }

    #[test]
    fn source_of_returns_correct_attribution() {
        let router = empty_router();
        router.apply_toolset_tools(vec![t("Bash")]).unwrap();
        assert_eq!(router.source_of("Bash"), Some(Source::Toolset));
        // Skill/Agent are built-in runtime tools.
        assert_eq!(router.source_of("Skill"), Some(Source::Runtime));
        assert_eq!(router.source_of("Agent"), Some(Source::Runtime));
        assert_eq!(router.source_of("Ghost"), None);
    }

    // The dispatch path forwards the turn's cancellation signal to the
    // dispatched work at the ROUTER hop — `ToolRouter::call_tool` forwarding the
    // caller's `cancel` into `runtime_tools::dispatch(...)` in the
    // `Source::Runtime` arm. The sibling test
    // `runtime_tools::dispatch_forwards_cancel_to_the_agent_arm` only proves the
    // lower `dispatch() -> dispatch_agent()` hop; the router's forward of the
    // caller's token had NO coverage (mutating it to a fresh
    // `CancellationToken::new()` left the whole suite green).
    #[tokio::test]
    async fn call_tool_forwards_cancel_into_runtime_dispatch() {
        use crate::test_doubles::EndlessToolset;

        // A router whose Runtime arm reaches `runtime_tools::dispatch`: a kernel
        // with a `scout` persona so the in-process persona read succeeds and
        // execution reaches the cancellable sub-agent stream consumer.
        let root = tempfile::TempDir::new().unwrap().keep();
        std::fs::create_dir_all(root.join(WS).join("agents")).unwrap();
        std::fs::write(root.join(WS).join("agents/scout.md"), "scout persona").unwrap();
        let kernel = Arc::new(Kernel::new(root));
        let router: ToolRouter =
            ToolRouter::new(kernel, WS.to_string(), None, None, test_registry());

        // The sub-agent's model stream never terminates on its own — only a
        // fired, forwarded cancel can abandon it. If the router dropped the
        // caller's token (M4 at the `runtime_tools::dispatch(...)` forward) and
        // handed dispatch a fresh never-fired token, this drains forever
        // (hang/timeout) instead of returning Cancelled.
        let mut turn_seam = EndlessToolset;
        let cancel = CancellationToken::new();
        cancel.cancel(); // fired before the first poll

        let outcome = router
            .call_tool(
                "Agent",
                r#"{"name":"scout","query":"go"}"#,
                &mut turn_seam,
                "parent-conv",
                None,
                "tc",
                &cancel,
            )
            .await;

        // Behavioral assertion (CancellationToken has no PartialEq): observing
        // `DispatchAbort::Cancelled` proves the fired token crossed the router
        // hop and was seen by the dispatched sub-agent work.
        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "call_tool must forward the fired cancel into runtime dispatch, got {outcome:?}"
        );
    }

    // The `Source::Toolset` arm is the caller in the cascade: it learns the
    // call_id from `begin_tool_call`, then races `await_tool_result` against the
    // turn's cancel token. On cancel it issues exactly one fire-and-forget
    // `cancel_tool_call` and returns the terminal `DispatchAbort::Cancelled`;
    // uncancelled, it returns the toolset result unchanged and issues no cancel.
    //
    // The loop half — `Err(DispatchAbort::Cancelled)` driving `llm_loop` to
    // `LoopError::Cancelled` with no tool message appended — is already pinned,
    // source-agnostically, by agent.rs
    // `cancelled_subagent_terminates_loop_without_continuing`; not duplicated here.

    // When the turn's cancellation signal fires while the caller is awaiting a
    // toolset tool call's result, the caller issues exactly one cancel operation
    // for that call's identifier and does not block the turn awaiting the cancel
    // operation's completion.
    #[tokio::test]
    async fn toolset_cancel_fires_exactly_one_cancel_and_returns_cancelled() {
        use crate::test_doubles::FakeToolset;

        // `result: None` => await_tool_result pends forever. If the arm awaited
        // the result instead of racing (biased) the already-fired cancel, this
        // test would hang — that hang is the non-blocking clause's teeth.
        let toolset = FakeToolset::new("call-abc", None);
        let router: ToolRouter<FakeToolset> = ToolRouter::new(
            test_kernel(),
            WS.to_string(),
            Some(toolset.clone()),
            None,
            test_registry(),
        );
        router.apply_toolset_tools(vec![t("Bash")]).unwrap();

        let mut turn_seam = UnconfiguredToolset; // the Toolset arm never touches toolset
        let cancel = CancellationToken::new();
        cancel.cancel(); // fired before dispatch

        let outcome = router
            .call_tool("Bash", "{}", &mut turn_seam, "conv", None, "tc", &cancel)
            .await;

        // Materiality: folding Cancelled into `Ok(is_error=true)` instead of
        // returning it reds this.
        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "a fired cancel on an Toolset call must return Cancelled, got {outcome:?}"
        );

        // The cancel is fire-and-forget (spawned); poll briefly for it to land.
        let mut recorded = toolset.cancels();
        for _ in 0..200 {
            if !recorded.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            recorded = toolset.cancels();
        }
        // Materiality: firing zero cancels (dropped), two cancels, or a cancel
        // for the wrong id all red this exact-match.
        assert_eq!(
            recorded,
            vec!["call-abc".to_string()],
            "exactly one cancel for the begun call's id must be issued"
        );
    }

    // A toolset tool call that runs to completion without any cancellation
    // returns its result unchanged.
    #[tokio::test]
    async fn toolset_uncancelled_returns_result_unchanged() {
        use crate::test_doubles::FakeToolset;
        use proto_common::tool_result_frame::Frame;
        use proto_common::{ToolComplete, ToolOutcome, ToolResultFrame};

        // The toolset streams one stdout frame then the terminal; the arm folds
        // it into the model-facing result via `assemble_from_frames`.
        let scripted = vec![
            ToolResultFrame {
                frame: Some(Frame::Stdout("toolset output".into())),
            },
            ToolResultFrame {
                frame: Some(Frame::Complete(ToolComplete {
                    outcome: ToolOutcome::Done as i32,
                    exit_code: 0,
                })),
            },
        ];
        let toolset = FakeToolset::new("call-xyz", Some(scripted));
        let router: ToolRouter<FakeToolset> = ToolRouter::new(
            test_kernel(),
            WS.to_string(),
            Some(toolset.clone()),
            None,
            test_registry(),
        );
        router.apply_toolset_tools(vec![t("Bash")]).unwrap();

        let mut turn_seam = UnconfiguredToolset;
        let cancel = CancellationToken::new(); // never fired

        let resp = router
            .call_tool("Bash", "{}", &mut turn_seam, "conv", None, "tc", &cancel)
            .await
            .expect("an uncancelled toolset call returns its result");

        // Materiality: altering the result reds the output/is_error asserts; a
        // spurious cancel on the uncancelled path reds the empty-cancels assert.
        assert_eq!(crate::agent::collect_text(&resp.content), "toolset output");
        assert!(!resp.is_error);
        assert!(
            toolset.cancels().is_empty(),
            "no cancel may be issued when the turn was never cancelled"
        );
    }

    // The agent-turn Toolset arm appends each consumed frame to the execution
    // log as it arrives, so an agent-turn call is re-subscribable from the same
    // `.frames` record the client path writes — one store format, no separate
    // end-of-call write.
    //
    // Materiality: dropping the `append_frame` call in the consume loop leaves no
    // persisted record, reding the read-back; appending only the terminal (not
    // each frame as it arrives) reds the stdout-frame assertion.
    #[tokio::test]
    async fn toolset_arm_appends_consumed_frames_to_the_execution_log() {
        use crate::execution_log::{ExecutionLogWriter, LocalFsExecutionLog};
        use crate::test_doubles::FakeToolset;
        use proto_common::tool_result_frame::Frame;
        use proto_common::{ToolComplete, ToolOutcome, ToolResultFrame};

        let scripted = vec![
            ToolResultFrame {
                frame: Some(Frame::Stdout("agent-turn output".into())),
            },
            ToolResultFrame {
                frame: Some(Frame::Complete(ToolComplete {
                    outcome: ToolOutcome::Done as i32,
                    exit_code: 0,
                })),
            },
        ];
        let toolset = FakeToolset::new("call-agent-turn", Some(scripted));
        let dir = tempfile::tempdir().unwrap();
        let log: Arc<dyn ExecutionLogWriter> = Arc::new(LocalFsExecutionLog::new(
            dir.path().to_path_buf(),
            "test-conv".to_string(),
        ));
        let router: ToolRouter<FakeToolset> = ToolRouter::new(
            test_kernel(),
            WS.to_string(),
            Some(toolset),
            None,
            test_registry(),
        )
        .with_execution_log(log.clone());
        router.apply_toolset_tools(vec![t("Bash")]).unwrap();

        let mut turn_seam = UnconfiguredToolset;
        let cancel = CancellationToken::new(); // never fired

        router
            .call_tool("Bash", "{}", &mut turn_seam, "conv", None, "tc", &cancel)
            .await
            .expect("an uncancelled agent-turn call returns its result");

        let persisted = log
            .read("call-agent-turn")
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
    // CANCELED to the conversation's execution.json.
    //
    // The toolset RUNTIME emits its OWN terminal `ToolComplete{outcome: Canceled}`
    // when it SIGKILLs a mid-flight child; the harness does not fabricate it.
    // This test scripts the fake toolset to stream a partial stdout line then that
    // runtime CANCELED terminal, fires the turn's cancel, and asserts the runtime's
    // canceled terminal lands in the conversation's `execution.json`.
    //
    // Materiality: abandoning the `consume` future on cancel — the biased
    // `select!` cancel arm spawns the fire-and-forget `cancel_tool_call`, returns
    // `Cancelled`, and drops the stream — appends no CANCELED terminal, leaving
    // `read(call_id)` `None`. Draining the stream into `execution.json` after
    // cancel is what appends the terminal.
    //
    // DRAIN, not SYNTHESIZE: the scripted CANCELED terminal carries a distinctive
    // `exit_code` (137) — a provenance fingerprint neither the `failed_terminal()`
    // synthetic (Failed / -1) nor a fabricated bare `Canceled` terminal would
    // reproduce. The mutants this kills: appending `failed_terminal()` reds the
    // `outcome == Canceled` assert; synthesizing a bare `Canceled` terminal
    // (exit_code -1) reds the `exit_code == 137` assert; draining only the terminal
    // (skipping pre-terminal frames) reds the partial-stdout assert.
    #[tokio::test]
    async fn canceled_in_flight_call_appends_a_canceled_terminal_to_the_execution_log() {
        use crate::execution_log::{ExecutionLogWriter, LocalFsExecutionLog};
        use crate::test_doubles::FakeToolset;
        use proto_common::tool_result_frame::Frame;
        use proto_common::{ToolComplete, ToolOutcome, ToolResultFrame};

        // The runtime streams a partial output line, then — because its child was
        // SIGKILLed on the cancel — its own terminal with outcome Canceled. The
        // 137 exit_code is a provenance fingerprint: a fabricated terminal would
        // not carry it.
        let scripted = vec![
            ToolResultFrame {
                frame: Some(Frame::Stdout("partial output before cancel".into())),
            },
            ToolResultFrame {
                frame: Some(Frame::Complete(ToolComplete {
                    outcome: ToolOutcome::Canceled as i32,
                    exit_code: 137,
                })),
            },
        ];
        let toolset = FakeToolset::new("call-canceled", Some(scripted));
        let dir = tempfile::tempdir().unwrap();
        let log: Arc<dyn ExecutionLogWriter> = Arc::new(LocalFsExecutionLog::new(
            dir.path().to_path_buf(),
            "test-conv".to_string(),
        ));
        let router: ToolRouter<FakeToolset> = ToolRouter::new(
            test_kernel(),
            WS.to_string(),
            Some(toolset),
            None,
            test_registry(),
        )
        .with_execution_log(log.clone());
        router.apply_toolset_tools(vec![t("Bash")]).unwrap();

        let mut turn_seam = UnconfiguredToolset;
        let cancel = CancellationToken::new();
        cancel.cancel(); // fired before dispatch: the arm takes the cancel branch

        let outcome = router
            .call_tool("Bash", "{}", &mut turn_seam, "conv", None, "tc", &cancel)
            .await;
        // The turn still unwinds promptly on cancel; the drain runs detached.
        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "a fired cancel on an Toolset call still returns Cancelled promptly, got {outcome:?}"
        );

        // The CANCELED terminal is appended by a DETACHED drain in the fixed
        // version, so poll for it to land (bounded, like the sibling cancel test).
        let mut persisted = None;
        for _ in 0..200 {
            if let Some(call) = log.read("call-canceled").await {
                if call.has_terminal() {
                    persisted = Some(call);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let persisted = persisted.expect(
            "a canceled-in-flight call must append the runtime's CANCELED terminal to \
             execution.json; current prod abandons the consume future on cancel so nothing lands",
        );

        // The terminal record carries outcome CANCELED (the criterion) AND the
        // drained runtime frame's distinctive exit_code (proves DRAIN, not
        // synthesize).
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

    fn stdout_frame(s: &str) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(proto_common::tool_result_frame::Frame::Stdout(s.into())),
        }
    }

    fn done_terminal() -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(proto_common::tool_result_frame::Frame::Complete(
                proto_common::ToolComplete {
                    outcome: proto_common::ToolOutcome::Done as i32,
                    exit_code: 0,
                },
            )),
        }
    }

    /// Injected execution-log writer that parks the FIRST frame's persist until
    /// the test releases it. Lets the ordering test hold the append open and
    /// observe that no live subscriber sees the frame while its persist is still
    /// in flight.
    struct OrderingWriter {
        release: std::sync::Arc<tokio::sync::Notify>,
        gated: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl crate::execution_log::ExecutionLogWriter for OrderingWriter {
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
    // Materiality: the append fully completes before the fan-out send. Moving the
    // append after the send (or dropping the `.await` so it no longer completes
    // first) lets the fan-out deliver the frame while the parked persist is still
    // in flight, so the "observed nothing while parked" assert reds. The
    // post-release delivery assert is the tautology cut: it proves the
    // non-observation was the ordering gate, not a permanently dead stream (a stub
    // that never delivers would pass the first assert but red the second).
    #[tokio::test]
    async fn frame_is_persisted_before_a_live_subscriber_can_observe_it() {
        use crate::test_doubles::FakeToolset;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        use tokio::sync::Notify;

        let scripted = vec![stdout_frame("ORDERED"), done_terminal()];
        let toolset = FakeToolset::new("call-order", Some(scripted));
        let release = Arc::new(Notify::new());
        let writer: Arc<dyn crate::execution_log::ExecutionLogWriter> = Arc::new(OrderingWriter {
            release: release.clone(),
            gated: AtomicBool::new(false),
        });
        let router: ToolRouter<FakeToolset> = ToolRouter::new(
            test_kernel(),
            WS.to_string(),
            Some(toolset),
            None,
            test_registry(),
        )
        .with_execution_log(writer);
        // The client path resolves the tool's source, so the toolset tool under
        // test must be advertised — as it is in production, where the client can
        // only dispatch a tool the harness advertised to it.
        router.apply_toolset_tools(vec![t("Bash")]).unwrap();

        let call_id = router
            .dispatch_client_tool("Bash", "{}", "conv-order")
            .await
            .expect("dispatch mints the call and spawns its stream consumer");
        let mut stream = router
            .await_client_tool(&call_id, "conv-order")
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

        // Tautology cut: releasing the persist must then deliver that same frame,
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

    // A stream that ended WITHOUT a terminal frame retains every frame it
    // received, and a re-subscribe replays all of them (then a synthetic terminal
    // so the client's await still ends in an outcome). The record is written with
    // two stdout frames and no terminal, modelling a call whose live session died
    // mid-stream.
    //
    // Materiality: a miss-arm rewrite that replays only the last/terminal frame
    // instead of every retained frame reds the "both frames" assert; one that
    // omits the synthetic terminal on a no-terminal record reds the terminal
    // assert. Asserting BOTH received frames (not merely one, and not merely
    // non-error) is the tautology cut against a resolver that returns an empty
    // replay.
    #[tokio::test]
    async fn stream_ending_without_terminal_retains_every_received_frame_for_replay() {
        use proto_common::tool_result_frame::Frame;

        let reg = test_registry();
        let conv_id = reg.mint().await.unwrap();
        let writer = reg
            .execution_log_for(&conv_id)
            .await
            .expect("a local-fs conversation has an execution-log writer");
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
            ToolRouter::new(test_kernel(), WS.to_string(), None, None, reg.clone());
        let mut stream = router
            .await_client_tool("call-truncated", &conv_id)
            .await
            .expect("a died-mid-call record must resolve from disk and replay its retained frames");

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
            texts.iter().any(|t| t == "RETAINED-1") && texts.iter().any(|t| t == "RETAINED-2"),
            "every frame received before the stream ended is retained and replayed, got {texts:?}"
        );
        assert!(
            saw_terminal,
            "a truncated replay is closed with a synthetic terminal so the client's await ends in an outcome"
        );
    }

    // When a call's live session has already ended, a re-subscribe resolves the
    // owning conversation from the durable execution log and replays the recorded
    // frames — including the recorded terminal for a completed call.
    //
    // Materiality: the resolver walks the conversation directories on disk,
    // matches the call_id, and replays. A resolver that returns an empty replay
    // reds the "DISK-REPLAY" content assert; one that never resolves reds the
    // `.expect`. Asserting the specific recorded frame content is the tautology
    // cut against a vacuous (empty / non-error) replay.
    #[tokio::test]
    async fn retired_session_resubscribe_resolves_conversation_from_disk_and_replays() {
        use proto_common::tool_result_frame::Frame;

        let reg = test_registry();
        let conv_id = reg.mint().await.unwrap();
        let writer = reg
            .execution_log_for(&conv_id)
            .await
            .expect("a local-fs conversation has an execution-log writer");
        writer
            .append_frame("call-done", &stdout_frame("DISK-REPLAY"))
            .await
            .unwrap();
        writer
            .append_frame("call-done", &done_terminal())
            .await
            .unwrap();

        let router: ToolRouter =
            ToolRouter::new(test_kernel(), WS.to_string(), None, None, reg.clone());
        let mut stream = router
            .await_client_tool("call-done", &conv_id)
            .await
            .expect(
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

    // A finished call re-subscribed with its own conversation_id is served from
    // THAT conversation's execution.json by a direct single-file read — the passed
    // conversation_id selects the file. The same call_id awaited under a DIFFERENT
    // conversation resolves to NotFound: nothing scans the other conversations'
    // logs for it.
    //
    // Materiality: a mutant that ignores the passed conversation_id (e.g. reinstates
    // a walk of every conversation directory) would resolve `call-owned` under the
    // unrelated conversation too, reding the NotFound assert. A mutant that opens the
    // wrong conversation's file reds the owning-conversation replay assert.
    #[tokio::test]
    async fn resubscribe_reads_only_the_named_conversations_execution_log() {
        use proto_common::tool_result_frame::Frame;

        let reg = test_registry();
        let owner = reg.mint().await.unwrap();
        let other = reg.mint().await.unwrap();
        let writer = reg
            .execution_log_for(&owner)
            .await
            .expect("a local-fs conversation has an execution-log writer");
        writer
            .append_frame("call-owned", &stdout_frame("OWNED-REPLAY"))
            .await
            .unwrap();
        writer
            .append_frame("call-owned", &done_terminal())
            .await
            .unwrap();

        let router: ToolRouter =
            ToolRouter::new(test_kernel(), WS.to_string(), None, None, reg.clone());

        // Named with its owning conversation: resolves via a direct read and replays.
        let mut stream = router
            .await_client_tool("call-owned", &owner)
            .await
            .expect("the owning conversation's execution.json serves the call by a direct read");
        let mut texts = Vec::new();
        while let Some(item) = stream.next().await {
            if let Some(Frame::Stdout(s)) = item.expect("the replayed item is a frame").frame {
                texts.push(s);
            }
        }
        assert!(
            texts.iter().any(|t| t == "OWNED-REPLAY"),
            "the passed conversation_id opens exactly the owning conversation's log, got {texts:?}"
        );

        // Named with an unrelated conversation: the single-file read misses and no
        // scan falls back onto the owning conversation's log.
        let miss = router.await_client_tool("call-owned", &other).await;
        assert!(
            miss.is_err(),
            "a call awaited under a conversation that does not own it must be NotFound, not \
             resolved by scanning every conversation's log"
        );
    }

    // ---- Client-path source-attribution tests ----

    /// Drain a client await stream to end-of-stream and assemble the result the
    /// client would see, using the same fold the client applies.
    async fn drain_client_call(
        router: &ToolRouter,
        call_id: &str,
        conversation_id: &str,
    ) -> (CallToolResponse, Vec<ToolResultFrame>) {
        let mut stream = router
            .await_client_tool(call_id, conversation_id)
            .await
            .expect("the dispatched call must be awaitable");
        let mut frames = Vec::new();
        while let Some(item) = stream.next().await {
            frames.push(item.expect("the streamed item is a frame, not a status error"));
        }
        (assemble_from_frames(&frames), frames)
    }

    // The harness is the authority for every tool: it decides what exists and
    // what runs, and the only thing that varies is WHERE the work happens. A
    // `Runtime`-source tool dispatched by the CLIENT must therefore resolve
    // in-process from this workspace's kernel, exactly as the agent-turn path
    // does — not be delegated to the toolset controller, which has never heard
    // of it.
    //
    // `toolset: None` is load-bearing: only an in-process resolution can produce
    // a result at all. A dispatch that reaches the `Source::Toolset` arm fails
    // with "toolset client not configured", and one that reached a real
    // controller would come back `NotFound: unknown tool: Skills`.
    //
    // Materiality: deleting the `Source::Runtime` arm (falling through to the
    // toolset) reds the dispatch `.expect`. Synthesizing a terminal frame with no
    // stdout frame reds the parse. Asserting the PARSED name/description rather
    // than "some frames arrived" is the tautology cut against a vacuous empty
    // replay.
    #[tokio::test]
    async fn client_dispatch_serves_a_runtime_tool_in_process() {
        let root = tempfile::TempDir::new().unwrap().keep();
        std::fs::create_dir_all(root.join(WS).join("skills")).unwrap();
        std::fs::write(
            root.join(WS).join("skills/classify.md"),
            "# Classify\n\nDecide the doctype.\n",
        )
        .unwrap();
        let reg = test_registry();
        let conv_id = reg.mint().await.unwrap();
        let router: ToolRouter = ToolRouter::new(
            Arc::new(Kernel::new(root)),
            WS.to_string(),
            None,
            None,
            reg.clone(),
        );

        let call_id = router
            .dispatch_client_tool("Skills", r#"{"detail":true}"#, &conv_id)
            .await
            .expect("a Runtime-source tool must resolve in-process on the client path");

        let (resp, _) = drain_client_call(&router, &call_id, &conv_id).await;
        assert!(
            !resp.is_error,
            "a successful Skills listing is not an error"
        );
        let infos: Vec<serde_json::Value> =
            serde_json::from_str(&crate::agent::collect_text(&resp.content))
                .expect("the client-visible text is the runtime tool's JSON output");
        assert_eq!(infos.len(), 1, "one skill is present in the kernel");
        assert_eq!(infos[0]["name"], "classify");
        assert_eq!(infos[0]["description"], "Decide the doctype.");
    }

    // A `Runtime`-source tool that fails must end in a NON-DONE terminal. The
    // client derives its error bit solely from the terminal outcome, so a
    // hardcoded DONE would make the command menu render an error string as a
    // command list.
    //
    // Materiality: hardcoding `ToolOutcome::Done` in the synthesized terminal reds
    // the outcome assert; dropping the response's content when `is_error` is set
    // reds the message assert.
    #[tokio::test]
    async fn client_dispatch_of_a_failing_runtime_tool_ends_in_a_non_done_terminal() {
        use proto_common::tool_result_frame::Frame;

        let reg = test_registry();
        let conv_id = reg.mint().await.unwrap();
        let router: ToolRouter =
            ToolRouter::new(test_kernel(), WS.to_string(), None, None, reg.clone());

        let call_id = router
            .dispatch_client_tool("Skill", r#"{"name":"missing"}"#, &conv_id)
            .await
            .expect("a Runtime-source tool must resolve in-process on the client path");

        let (resp, frames) = drain_client_call(&router, &call_id, &conv_id).await;
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
            crate::agent::collect_text(&resp.content).contains("skill not found: missing"),
            "the runtime tool's error text reaches the client, got {:?}",
            resp.content
        );
        assert!(resp.is_error, "the assembled result carries the error bit");
    }

    /// Drive a dispatched client call to quiescence: its spawned task has
    /// published its terminal frame, or has already retired the session.
    ///
    /// This is a cooperative scheduling point, NOT a sleep. The loop exits on an
    /// OBSERVED terminal state, never on elapsed time. Under the current-thread
    /// test runtime a yield is sufficient: the runtime task's only await points
    /// are the in-process dispatch and the session-map write, neither of which
    /// waits on anything external.
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
    // call is long since complete. With no conversation named there is no
    // durable record to fall back on, so the in-memory session is the ONLY copy
    // of the result and must still be servable.
    //
    // This is the production command-menu path: the menu opens with no
    // conversation selected, dispatches `Skills`, then awaits.
    //
    // Materiality: retiring the session as soon as the task finishes drops the
    // only copy of the frames, and the late subscriber takes the miss arm, finds
    // no execution log for an empty conversation_id, and gets `NotFound`. The
    // three sibling tests all subscribe inline in the same task, often before
    // the spawned task is first polled, so none of them exercise this gap.
    // Asserting the PARSED output is the tautology cut against a replay that
    // resolves but carries nothing.
    #[tokio::test]
    async fn completed_runtime_call_is_servable_to_a_late_subscriber_with_no_conversation() {
        let root = tempfile::TempDir::new().unwrap().keep();
        std::fs::create_dir_all(root.join(WS).join("skills")).unwrap();
        std::fs::write(
            root.join(WS).join("skills/classify.md"),
            "# Classify\n\nDecide the doctype.\n",
        )
        .unwrap();
        let router: ToolRouter = ToolRouter::new(
            Arc::new(Kernel::new(root)),
            WS.to_string(),
            None,
            None,
            test_registry(),
        );

        // Empty conversation_id: no execution log is derived at dispatch, so
        // nothing durable can serve this call later.
        let call_id = router
            .dispatch_client_tool("Skills", r#"{"detail":true}"#, "")
            .await
            .expect("a Runtime-source tool must resolve in-process on the client path");

        // The gap: the call completes before the client's second RPC arrives.
        settle_client_call(&router, &call_id).await;

        let (resp, _) = drain_client_call(&router, &call_id, "").await;
        assert!(
            !resp.is_error,
            "a successful Skills listing is not an error"
        );
        let infos: Vec<serde_json::Value> =
            serde_json::from_str(&crate::agent::collect_text(&resp.content))
                .expect("the late subscriber receives the runtime tool's JSON output");
        assert_eq!(infos.len(), 1, "one skill is present in the kernel");
        assert_eq!(infos[0]["name"], "classify");
        assert_eq!(infos[0]["description"], "Decide the doctype.");
    }

    // `Channel`-source tools execute on the user's device via a `ServerRequest`.
    // A client dispatching one would be a request looping back at its own sender,
    // so the client path rejects it by name instead of falling through to the
    // toolset, which would answer with a confusing `unknown tool`.
    //
    // Materiality: falling through to the toolset arm reds both asserts — the
    // dispatch would succeed (the fake mints a call_id) and the fake would record
    // the begin.
    #[tokio::test]
    async fn client_dispatch_rejects_a_channel_tool_without_calling_the_toolset() {
        use crate::test_doubles::FakeToolset;

        let toolset = FakeToolset::new("call-should-never-begin", None);
        let router: ToolRouter<FakeToolset> = ToolRouter::new(
            test_kernel(),
            WS.to_string(),
            Some(toolset.clone()),
            None,
            test_registry(),
        );

        let err = router
            .dispatch_client_tool("RevealPath", r#"{"path":"/x"}"#, "conv")
            .await
            .expect_err("a Channel-source tool is not client-dispatchable");
        assert!(
            err.contains("not client-dispatchable"),
            "the error names the tool as not client-dispatchable, got {err:?}"
        );
        assert!(
            toolset.begins().is_empty(),
            "a rejected Channel dispatch must not reach the toolset, got {:?}",
            toolset.begins()
        );
    }

    // A re-subscribe resolves the conversation and replays its frames WITHOUT
    // relying on any in-memory table populated only at dispatch. Process 1
    // dispatches a client tool (frames persist to the durable log); a brand-new
    // registry + router over the SAME disk — modelling a harness restart with
    // no dispatch-time state — must still resolve+replay.
    //
    // Materiality: resolution reads the durable log, not in-memory dispatch state.
    // Reintroducing ANY dispatch-only in-memory dependency reds this (process 2
    // never dispatched); a resolver that returns an empty replay reds the content
    // assert.
    #[tokio::test]
    async fn resubscribe_survives_a_harness_restart_resolving_from_disk_only() {
        use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
        use crate::test_doubles::FakeToolset;
        use proto_common::tool_result_frame::Frame;

        let root = tempfile::TempDir::new().unwrap().keep();

        // Process 1: dispatch a client tool; its frames persist to disk.
        let call_id;
        let conv_id;
        {
            let factory: Arc<dyn ConversationStoreFactory> =
                Arc::new(LocalFsFactory::new(root.clone()));
            let reg = Arc::new(ConversationRegistry::new(factory));
            conv_id = reg.mint().await.unwrap();
            let scripted = vec![stdout_frame("SURVIVES-RESTART"), done_terminal()];
            let toolset = FakeToolset::new("call-restart", Some(scripted));
            let router: ToolRouter<FakeToolset> = ToolRouter::new(
                test_kernel(),
                WS.to_string(),
                Some(toolset),
                None,
                reg.clone(),
            );
            router.apply_toolset_tools(vec![t("Bash")]).unwrap();
            call_id = router
                .dispatch_client_tool("Bash", "{}", &conv_id)
                .await
                .expect("dispatch mints the call and persists its frames");

            // Wait until the dispatched call's terminal is durable on disk.
            let writer = reg
                .execution_log_for(&conv_id)
                .await
                .expect("the conversation has an execution-log writer");
            let mut durable = false;
            for _ in 0..400 {
                if let Some(call) = writer.read(&call_id).await {
                    if call.has_terminal() {
                        durable = true;
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert!(
                durable,
                "the dispatched call must persist to the durable log before the restart"
            );
        }

        // Process 2 (restart): a fresh registry + router over the same disk, with
        // NO dispatch-time in-memory state. Re-subscribe must still resolve+replay.
        let factory: Arc<dyn ConversationStoreFactory> =
            Arc::new(LocalFsFactory::new(root.clone()));
        let reg = Arc::new(ConversationRegistry::new(factory));
        let router: ToolRouter = ToolRouter::new(test_kernel(), WS.to_string(), None, None, reg);
        let mut stream = router.await_client_tool(&call_id, &conv_id).await.expect(
            "after restart the call resolves from disk, not a dispatch-populated in-memory table",
        );

        let mut texts = Vec::new();
        while let Some(item) = stream.next().await {
            if let Some(Frame::Stdout(s)) = item.expect("the replayed item is a frame").frame {
                texts.push(s);
            }
        }
        assert!(
            texts.iter().any(|t| t == "SURVIVES-RESTART"),
            "the frames recorded before the restart replay after it, got {texts:?}"
        );
    }
}
