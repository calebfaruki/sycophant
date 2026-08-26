//! Shared `#[cfg(test)]` test doubles reachable across the crate's unit-test
//! modules. `FakeToolset` backs the `Source::Toolset` arm without a live gRPC
//! server; `EndlessToolset`/`EndlessSource` drive the cancellation tests that
//! run a never-terminating sub-agent stream through the router.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use proto_common::ToolResultFrame;
use toolset_proto::{turn_event, ContentDelta, TurnEvent, TurnRequest};

use crate::clients::{ToolResultStream, ToolsetRpc, TurnSource};
use crate::kernel::Kernel;

/// The workspace name used by test routers/kernels.
pub(crate) const TEST_WS: &str = "ws";

/// An empty-workspace kernel over a throwaway temp dir. The dir is leaked so
/// the returned `Arc<Kernel>` can outlive this call; tests that need populated
/// content build their own `Kernel` directly.
pub(crate) fn test_kernel() -> Arc<Kernel> {
    let root = tempfile::TempDir::new().unwrap().keep();
    std::fs::create_dir_all(root.join(TEST_WS)).unwrap();
    Arc::new(Kernel::new(root))
}

/// One recorded `begin_tool_call`: tool name, the input JSON that went on the
/// wire, and the grant it carried.
pub(crate) type RecordedBegin = (String, String, Option<String>);

/// Fake toolset controller backing the begin/await/cancel split without a live
/// gRPC server. `Clone` (via a shared `Arc<Mutex<..>>` cancel recorder) so the
/// router's per-dispatch clone and the fire-and-forget cancel spawn share one
/// recorder. The `ToolRouter<A>` toolset generic lets tests back the
/// `Source::Toolset` arm with this.
#[derive(Clone)]
pub(crate) struct FakeToolset {
    /// The call_id `begin_tool_call` hands back.
    pub call_id: String,
    /// `await_tool_result` server-streams these frames; `None` pends forever so
    /// a racing cancel is the only way the arm can return.
    pub frames: Option<Vec<ToolResultFrame>>,
    /// Every `cancel_tool_call(call_id)` the arm issues, in order.
    pub cancels: Arc<Mutex<Vec<String>>>,
    /// Every `begin_tool_call(name, input_json, grant)` the arm issues, in
    /// order. Lets a test prove a dispatch was resolved WITHOUT delegating to
    /// the toolset.
    pub begins: Arc<Mutex<Vec<RecordedBegin>>>,
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

impl FakeToolset {
    pub(crate) fn new(call_id: &str, frames: Option<Vec<ToolResultFrame>>) -> Self {
        Self {
            call_id: call_id.to_string(),
            frames,
            cancels: Arc::new(Mutex::new(Vec::new())),
            begins: Arc::new(Mutex::new(Vec::new())),
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

    /// Snapshot of the `begin_tool_call`s issued so far, as
    /// `(name, input_json, grant)`.
    pub(crate) fn begins(&self) -> Vec<RecordedBegin> {
        self.begins.lock().unwrap().clone()
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
impl ToolsetRpc for FakeToolset {
    async fn turn(&mut self, _request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
        Err("FakeToolset: turn unused in tool-dispatch tests".into())
    }

    async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
        Ok(())
    }

    async fn watch_tools(
        &mut self,
    ) -> Result<tonic::Streaming<proto_common::ToolListUpdate>, String> {
        Err("FakeToolset: watch_tools unused in tool-dispatch tests".into())
    }

    async fn begin_tool_call(
        &mut self,
        name: &str,
        input_json: &str,
        grant: Option<&str>,
    ) -> Result<String, String> {
        self.begins.lock().unwrap().push((
            name.to_string(),
            input_json.to_string(),
            grant.map(str::to_string),
        ));
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

/// A toolset whose every turn yields a source that never terminates — used to
/// prove a fired cancel abandons an in-flight sub-agent stream instead of
/// draining it. Drives the router-level cancellation test.
pub(crate) struct EndlessToolset;

#[async_trait]
impl ToolsetRpc for EndlessToolset {
    async fn turn(&mut self, _request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
        Ok(Box::new(EndlessSource))
    }
    async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn watch_tools(
        &mut self,
    ) -> Result<tonic::Streaming<proto_common::ToolListUpdate>, String> {
        Err("EndlessToolset: watch_tools unused".into())
    }
    async fn begin_tool_call(
        &mut self,
        _n: &str,
        _i: &str,
        _grant: Option<&str>,
    ) -> Result<String, String> {
        Err("EndlessToolset: begin_tool_call unused".into())
    }
    async fn await_tool_result(
        &mut self,
        _call_id: &str,
    ) -> Result<Box<dyn ToolResultStream>, String> {
        Err("EndlessToolset: await_tool_result unused".into())
    }
    async fn cancel_tool_call(&mut self, _call_id: &str) -> Result<bool, String> {
        Err("EndlessToolset: cancel_tool_call unused".into())
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
