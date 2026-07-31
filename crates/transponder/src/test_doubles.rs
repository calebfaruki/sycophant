//! Shared `#[cfg(test)]` test doubles reachable across the crate's unit-test
//! modules. Lifting `FakeMainframe` here lets `tool_router`'s tests construct a
//! `ToolRouter::<FakeMainframe>::new(...)` — the generic mainframe seam — while
//! `runtime_tools`'s tests reuse the same fake. `EndlessHangar`/`EndlessSource`
//! are provisioned for the cancellation tests that drive a never-terminating
//! sub-agent stream through the router.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hangar_proto::{turn_event, ContentDelta, TurnEvent, TurnRequest};
use mainframe_proto::AgentInfo;
use proto_common::CallToolResponse;
use proto_common::ToolResultFrame;

use crate::clients::{AirlockRpc, HangarRpc, MainframeRpc, ToolResultStream, TurnSource};

/// Fake airlock controller backing the begin/await/cancel split without a live
/// gRPC server. `Clone` (via a shared `Arc<Mutex<..>>` cancel recorder) so the
/// router's per-dispatch clone and the fire-and-forget cancel spawn share one
/// recorder. Mirrors the `FakeMainframe` seam: a `ToolRouter<M, A>` generic over
/// the airlock RPC lets tests back the `Source::Airlock` arm with this.
#[derive(Clone)]
pub(crate) struct FakeAirlock {
    /// The call_id `begin_tool_call` hands back.
    pub call_id: String,
    /// `await_tool_result` server-streams these frames; `None` pends forever so
    /// a racing cancel is the only way the arm can return.
    pub frames: Option<Vec<ToolResultFrame>>,
    /// Every `cancel_tool_call(call_id)` the arm issues, in order.
    pub cancels: Arc<Mutex<Vec<String>>>,
    /// Optional release gate. When set, the scripted stream parks on it (a real
    /// await point) just before yielding the terminal `ToolComplete`, so the
    /// call stays genuinely in flight — present in the router's session map,
    /// with no terminal recorded yet — until the test fires the gate. Lets a
    /// test drive the live in-flight follow path rather than the
    /// already-retired persisted-fallback path.
    pub gate: Option<Arc<tokio::sync::Notify>>,
    /// How the scripted stream ends once its frames drain. `Eof` yields end-of-
    /// stream; `ErrAfterGate` parks on the gate then yields a frame-stream error
    /// with no terminal frame, so the consumer breaks and retires the session
    /// without ever emitting a terminal — the abnormal-close case.
    pub end: StreamEnd,
}

/// How a scripted stream terminates after its frames drain.
#[derive(Clone)]
pub(crate) enum StreamEnd {
    /// Yield `None` (end of stream) once frames drain — the two-arg
    /// constructor's default.
    Eof,
    /// Park on the gate once frames drain, then yield `Some(Err(message))`
    /// exactly once — a mid-stream error with no terminal frame. Drives the
    /// abnormal-end follow path: the session consumer breaks on the error and
    /// retires the session without a terminal.
    ErrAfterGate(String),
}

impl FakeAirlock {
    pub(crate) fn new(call_id: &str, frames: Option<Vec<ToolResultFrame>>) -> Self {
        Self {
            call_id: call_id.to_string(),
            frames,
            cancels: Arc::new(Mutex::new(Vec::new())),
            gate: None,
            end: StreamEnd::Eof,
        }
    }

    /// Once the scripted frames drain, park on the gate then yield a single
    /// frame-stream error (never a terminal). Pair with `with_gate` so the call
    /// stays genuinely in flight until the test releases the gate, then closes
    /// abnormally with no terminal frame.
    pub(crate) fn erring_after_gate(mut self, message: &str) -> Self {
        self.end = StreamEnd::ErrAfterGate(message.to_string());
        self
    }

    /// Park the scripted stream on `gate` just before the terminal frame, so the
    /// call is still in flight when a late subscriber arrives. The shared
    /// `Arc<Notify>` lets the test release the terminal after it has subscribed.
    pub(crate) fn with_gate(mut self, gate: Arc<tokio::sync::Notify>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// Snapshot of the cancels issued so far.
    pub(crate) fn cancels(&self) -> Vec<String> {
        self.cancels.lock().unwrap().clone()
    }
}

/// A scripted frame stream: `Some` yields each frame then EOF; `None` pends
/// forever so a racing cancel is the only exit. When `gate` is set, the stream
/// awaits it once, immediately before the terminal frame, so the consumer parks
/// mid-call until the test releases it.
struct ScriptedFrameStream {
    frames: Option<VecDeque<ToolResultFrame>>,
    gate: Option<Arc<tokio::sync::Notify>>,
    end: StreamEnd,
}

impl ScriptedFrameStream {
    fn is_terminal(frame: &ToolResultFrame) -> bool {
        matches!(
            frame.frame,
            Some(proto_common::tool_result_frame::Frame::Complete(_))
        )
    }
}

#[async_trait]
impl ToolResultStream for ScriptedFrameStream {
    async fn next_frame(&mut self) -> Option<Result<ToolResultFrame, String>> {
        match &mut self.frames {
            Some(q) => match q.front() {
                Some(front) => {
                    if Self::is_terminal(front) {
                        if let Some(gate) = self.gate.take() {
                            gate.notified().await;
                        }
                    }
                    q.pop_front().map(Ok)
                }
                // Frames drained: end normally, or park then error abnormally.
                None => match std::mem::replace(&mut self.end, StreamEnd::Eof) {
                    StreamEnd::Eof => None,
                    StreamEnd::ErrAfterGate(message) => {
                        if let Some(gate) = self.gate.take() {
                            gate.notified().await;
                        }
                        Some(Err(message))
                    }
                },
            },
            None => {
                std::future::pending::<()>().await;
                unreachable!("next_frame pends forever when no frames are configured")
            }
        }
    }
}

#[async_trait]
impl AirlockRpc for FakeAirlock {
    async fn begin_tool_call(&mut self, _name: &str, _input_json: &str) -> Result<String, String> {
        Ok(self.call_id.clone())
    }

    async fn await_tool_result(
        &mut self,
        _call_id: &str,
    ) -> Result<Box<dyn ToolResultStream>, String> {
        Ok(Box::new(ScriptedFrameStream {
            frames: self.frames.clone().map(VecDeque::from),
            gate: self.gate.clone(),
            end: self.end.clone(),
        }))
    }

    async fn cancel_tool_call(&mut self, call_id: &str) -> Result<bool, String> {
        self.cancels.lock().unwrap().push(call_id.to_string());
        Ok(true)
    }
}

/// Fake mainframe backing `get_agent`/`list_agents`/`call_tool` from in-memory
/// maps. `Clone` so it can seed a generic `ToolRouter<FakeMainframe>` (the
/// router clones its mainframe handle per dispatch).
#[derive(Clone, Default)]
pub(crate) struct FakeMainframe {
    pub agents_by_name: HashMap<String, String>,
    pub listed: Vec<AgentInfo>,
}

#[async_trait]
impl MainframeRpc for FakeMainframe {
    async fn get_agent(&mut self, name: &str) -> Result<String, String> {
        self.agents_by_name
            .get(name)
            .cloned()
            .ok_or_else(|| format!("FakeMainframe: no agent for {name}"))
    }
    async fn list_agents(&mut self) -> Result<Vec<AgentInfo>, String> {
        Ok(self.listed.clone())
    }
    async fn call_tool(
        &mut self,
        name: &str,
        _input_json: &str,
    ) -> Result<CallToolResponse, String> {
        // Not exercised by the current suite; a canned success keeps the
        // Mainframe-source dispatch arm satisfiable when a test routes to it.
        Ok(CallToolResponse {
            content: vec![proto_common::text_block(format!(
                "FakeMainframe::call_tool({name})"
            ))],
            is_error: false,
        })
    }
}

/// A hangar whose every turn yields a source that never terminates — used to
/// prove a fired cancel abandons an in-flight sub-agent stream instead of
/// draining it. Drives the router-level cancellation test.
pub(crate) struct EndlessHangar;

#[async_trait]
impl HangarRpc for EndlessHangar {
    async fn turn(&mut self, _request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
        Ok(Box::new(EndlessSource))
    }
    async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
        Ok(())
    }
}

/// A turn source that keeps emitting content deltas forever; only a fired
/// cancel can stop a consumer reading it.
pub(crate) struct EndlessSource;

#[async_trait]
impl TurnSource for EndlessSource {
    async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
        Some(Ok(TurnEvent {
            event: Some(turn_event::Event::ContentDelta(ContentDelta {
                text: "more".into(),
            })),
        }))
    }
}
