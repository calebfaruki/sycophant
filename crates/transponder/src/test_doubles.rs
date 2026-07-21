//! Shared `#[cfg(test)]` test doubles reachable across the crate's unit-test
//! modules. Lifting `FakeMainframe` here lets `tool_router`'s tests construct a
//! `ToolRouter::<FakeMainframe>::new(...)` — the generic mainframe seam — while
//! `runtime_tools`'s tests reuse the same fake. `EndlessHangar`/`EndlessSource`
//! are provisioned for the cancellation tests that drive a never-terminating
//! sub-agent stream through the router.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hangar_proto::{turn_event, ContentDelta, TurnEvent, TurnRequest};
use mainframe_proto::AgentInfo;
use proto_common::CallToolResponse;

use crate::clients::{AirlockRpc, HangarRpc, MainframeRpc, TurnSource};

/// Fake airlock controller backing the begin/await/cancel split without a live
/// gRPC server. `Clone` (via a shared `Arc<Mutex<..>>` cancel recorder) so the
/// router's per-dispatch clone and the fire-and-forget cancel spawn share one
/// recorder. Mirrors the `FakeMainframe` seam: a `ToolRouter<M, A>` generic over
/// the airlock RPC lets tests back the `Source::Airlock` arm with this.
#[derive(Clone)]
pub(crate) struct FakeAirlock {
    /// The call_id `begin_tool_call` hands back.
    pub call_id: String,
    /// `await_tool_result` returns `Some` verbatim; `None` pends forever so a
    /// racing cancel is the only way the arm can return.
    pub result: Option<CallToolResponse>,
    /// Every `cancel_tool_call(call_id)` the arm issues, in order.
    pub cancels: Arc<Mutex<Vec<String>>>,
}

impl FakeAirlock {
    pub(crate) fn new(call_id: &str, result: Option<CallToolResponse>) -> Self {
        Self {
            call_id: call_id.to_string(),
            result,
            cancels: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Snapshot of the cancels issued so far.
    pub(crate) fn cancels(&self) -> Vec<String> {
        self.cancels.lock().unwrap().clone()
    }
}

#[async_trait]
impl AirlockRpc for FakeAirlock {
    async fn begin_tool_call(&mut self, _name: &str, _input_json: &str) -> Result<String, String> {
        Ok(self.call_id.clone())
    }

    async fn await_tool_result(&mut self, _call_id: &str) -> Result<CallToolResponse, String> {
        match &self.result {
            Some(r) => Ok(r.clone()),
            None => {
                std::future::pending::<()>().await;
                unreachable!("await_tool_result pends forever when no result is configured")
            }
        }
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
            output: format!("FakeMainframe::call_tool({name})"),
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
