//! Shared `#[cfg(test)]` test doubles reachable across the crate's unit-test
//! modules. `FakeToolset` backs the `ToolRouter<A>` toolset generic without a
//! live gRPC server; `EndlessToolset`/`EndlessSource` drive the cancellation
//! tests that run a never-terminating sub-agent stream through the router.

use std::sync::Arc;

use async_trait::async_trait;
use toolset_proto::{turn_event, ContentDelta, TurnEvent, TurnRequest};

use crate::clients::{ToolsetRpc, TurnSource};
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

/// A toolset stub backing the `ToolRouter<A>` generic in tests that exercise the
/// runtime and channel paths. Tool dispatch runs in-process through
/// `DispatchState`, not this seam, so the tool-call RPCs are gone; only the
/// sub-agent turn surface remains.
#[derive(Clone)]
pub(crate) struct FakeToolset;

#[async_trait]
impl ToolsetRpc for FakeToolset {
    async fn turn(&mut self, _request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
        Err("FakeToolset: turn unused in these tests".into())
    }

    async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
        Ok(())
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
