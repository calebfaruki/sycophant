//! The merged `ToolsetController` service serves the tool half only: tool
//! dispatch and tool discovery. The controller no longer brokers a model turn.
//!
//! This is a compile-time guard. The impl below satisfies the generated
//! `ToolsetController` trait with exactly five methods. If the proto were to
//! declare an inference RPC again (`Turn`, `CancelTurn`, `GetTurn`,
//! `StreamTurnResult`, or `AwaitTurnCancel`), the trait would demand it and this
//! impl would fail to compile with "not all trait items implemented". The proto
//! declaring exactly the tool surface is the assertion.

use std::pin::Pin;

use futures::Stream;
use tonic::{Request, Response, Status};

use toolset_proto::toolset_controller_server::ToolsetController;
use toolset_proto::{
    AwaitToolResultRequest, CallToolRequest, CancelToolCallRequest, CancelToolCallResponse,
    ReportDiscoveredToolsAck, ReportDiscoveredToolsRequest, ToolCallHandle, ToolList,
    ToolResultFrame, WatchToolsRequest,
};

struct ToolSurfaceOnly;

#[tonic::async_trait]
impl ToolsetController for ToolSurfaceOnly {
    type WatchToolsStream = Pin<Box<dyn Stream<Item = Result<ToolList, Status>> + Send + 'static>>;

    type AwaitToolResultStream =
        Pin<Box<dyn Stream<Item = Result<ToolResultFrame, Status>> + Send + 'static>>;

    async fn watch_tools(
        &self,
        _request: Request<WatchToolsRequest>,
    ) -> Result<Response<Self::WatchToolsStream>, Status> {
        unimplemented!("compile-time guard only")
    }

    async fn begin_tool_call(
        &self,
        _request: Request<CallToolRequest>,
    ) -> Result<Response<ToolCallHandle>, Status> {
        unimplemented!("compile-time guard only")
    }

    async fn await_tool_result(
        &self,
        _request: Request<AwaitToolResultRequest>,
    ) -> Result<Response<Self::AwaitToolResultStream>, Status> {
        unimplemented!("compile-time guard only")
    }

    async fn cancel_tool_call(
        &self,
        _request: Request<CancelToolCallRequest>,
    ) -> Result<Response<CancelToolCallResponse>, Status> {
        unimplemented!("compile-time guard only")
    }

    async fn report_discovered_tools(
        &self,
        _request: Request<ReportDiscoveredToolsRequest>,
    ) -> Result<Response<ReportDiscoveredToolsAck>, Status> {
        unimplemented!("compile-time guard only")
    }
}

/// The impl above compiling is the guard; this test pins it into the run so a
/// regression that reintroduces an inference RPC surfaces as a build failure.
#[test]
fn the_controller_trait_is_satisfied_by_the_tool_surface_alone() {
    let _guard = ToolSurfaceOnly;
}
