//! An in-process harness stand-in on a loopback port, for the gateway's
//! forwarding paths.

use std::sync::{Arc, Mutex};

use harness_proto::harness_control_server::{HarnessControl, HarnessControlServer};
use proto_common::{
    AwaitToolResultRequest, CallToolRequest, CancelToolRequest, CancelToolResponse,
    CancelTurnRequest, CancelTurnResponse, ClearContextRequest, ClearContextResponse,
    CompactContextRequest, CompactContextResponse, DispatchToolResponse,
    GetConversationHistoryRequest, GetConversationHistoryResponse, TakeBackMessageRequest,
    TakeBackMessageResponse, ToolListUpdate, ToolResultFrame, WatchToolsRequest,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

/// The call_id the mock harness mints for every dispatch.
pub(crate) const MOCK_CALL_ID: &str = "call-42";

/// The tool forwarders, `get_conversation_history`, `take_back_message`,
/// `clear_context` and `compact_context` record the request they were handed, so a test can assert what the relay
/// passed through; `watch_tools` and `await_tool_result` stream whatever the
/// test sends on the matching channel. Every other method is unimplemented, so
/// a test that reaches one has escaped the check it meant to exercise and fails
/// on the wrong status.
#[derive(Default)]
pub(crate) struct MockHarness {
    pub(crate) tool_updates: Mutex<Option<mpsc::Receiver<Result<ToolListUpdate, Status>>>>,
    pub(crate) result_frames: Mutex<Option<mpsc::Receiver<Result<ToolResultFrame, Status>>>>,
    pub(crate) seen_dispatch: Arc<Mutex<Option<CallToolRequest>>>,
    pub(crate) seen_await: Arc<Mutex<Option<AwaitToolResultRequest>>>,
    pub(crate) seen_cancel: Arc<Mutex<Option<CancelToolRequest>>>,
    pub(crate) seen_history: Arc<Mutex<Option<GetConversationHistoryRequest>>>,
    pub(crate) seen_take_back: Arc<Mutex<Option<TakeBackMessageRequest>>>,
    pub(crate) seen_clear: Arc<Mutex<Option<ClearContextRequest>>>,
    pub(crate) seen_compact: Arc<Mutex<Option<CompactContextRequest>>>,
}

/// Take the stream a test armed, or refuse when it armed none.
#[allow(clippy::result_large_err)]
fn armed<T>(slot: &Mutex<Option<mpsc::Receiver<T>>>) -> Result<ReceiverStream<T>, Status> {
    slot.lock()
        .unwrap()
        .take()
        .map(ReceiverStream::new)
        .ok_or_else(|| Status::unimplemented("mock harness: no stream armed"))
}

#[tonic::async_trait]
impl HarnessControl for MockHarness {
    type WatchToolsStream = ReceiverStream<Result<ToolListUpdate, Status>>;
    type AwaitToolResultStream = ReceiverStream<Result<ToolResultFrame, Status>>;

    async fn watch_tools(
        &self,
        _request: Request<WatchToolsRequest>,
    ) -> Result<Response<Self::WatchToolsStream>, Status> {
        armed(&self.tool_updates).map(Response::new)
    }

    async fn dispatch_tool(
        &self,
        request: Request<CallToolRequest>,
    ) -> Result<Response<DispatchToolResponse>, Status> {
        *self.seen_dispatch.lock().unwrap() = Some(request.into_inner());
        Ok(Response::new(DispatchToolResponse {
            call_id: MOCK_CALL_ID.into(),
        }))
    }

    async fn await_tool_result(
        &self,
        request: Request<AwaitToolResultRequest>,
    ) -> Result<Response<Self::AwaitToolResultStream>, Status> {
        *self.seen_await.lock().unwrap() = Some(request.into_inner());
        armed(&self.result_frames).map(Response::new)
    }

    async fn cancel_tool(
        &self,
        request: Request<CancelToolRequest>,
    ) -> Result<Response<CancelToolResponse>, Status> {
        *self.seen_cancel.lock().unwrap() = Some(request.into_inner());
        Ok(Response::new(CancelToolResponse { cancelled: true }))
    }

    async fn get_conversation_history(
        &self,
        request: Request<GetConversationHistoryRequest>,
    ) -> Result<Response<GetConversationHistoryResponse>, Status> {
        *self.seen_history.lock().unwrap() = Some(request.into_inner());
        Ok(Response::new(GetConversationHistoryResponse::default()))
    }

    async fn take_back_message(
        &self,
        request: Request<TakeBackMessageRequest>,
    ) -> Result<Response<TakeBackMessageResponse>, Status> {
        *self.seen_take_back.lock().unwrap() = Some(request.into_inner());
        Ok(Response::new(TakeBackMessageResponse::default()))
    }

    async fn clear_context(
        &self,
        request: Request<ClearContextRequest>,
    ) -> Result<Response<ClearContextResponse>, Status> {
        *self.seen_clear.lock().unwrap() = Some(request.into_inner());
        Ok(Response::new(ClearContextResponse {}))
    }

    async fn compact_context(
        &self,
        request: Request<CompactContextRequest>,
    ) -> Result<Response<CompactContextResponse>, Status> {
        *self.seen_compact.lock().unwrap() = Some(request.into_inner());
        Ok(Response::new(CompactContextResponse {}))
    }

    async fn cancel_turn(
        &self,
        _request: Request<CancelTurnRequest>,
    ) -> Result<Response<CancelTurnResponse>, Status> {
        Err(Status::unimplemented("mock harness"))
    }
}

/// Serve `mock` on an ephemeral loopback port and return its address.
pub(crate) async fn spawn_mock_harness(mock: MockHarness) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(HarnessControlServer::new(mock))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
    );
    addr
}
