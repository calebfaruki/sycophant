//! Shared `#[cfg(test)]` test doubles reachable across the crate's unit-test
//! modules. Lifting `FakeMainframe` here lets `tool_router`'s tests construct a
//! `ToolRouter::<FakeMainframe>::new(...)` — the generic mainframe seam — while
//! `runtime_tools`'s tests reuse the same fake. `EndlessHangar`/`EndlessSource`
//! are provisioned for the cancellation tests that drive a never-terminating
//! sub-agent stream through the router.

use std::collections::HashMap;

use async_trait::async_trait;
use hangar_proto::{turn_event, ContentDelta, TurnEvent, TurnRequest};
use mainframe_proto::AgentInfo;
use proto_common::CallToolResponse;

use crate::clients::{HangarRpc, MainframeRpc, TurnSource};

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
