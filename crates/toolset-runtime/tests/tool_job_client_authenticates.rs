//! Integration test: the tool job's controller client attaches
//! `SaTokenInterceptor`, so every RPC carries the pod's projected SA token as a
//! Bearer header. Drives the real `connect_authenticated` seam against a fake
//! `ToolsetController` that rejects any call missing the Bearer token.
//!
//! Materiality: reverting `connect_authenticated` to a plain
//! `ToolsetControllerClient::connect` (the pre-fix bug) makes the fake server
//! return `missing authorization metadata` and this test fails.

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use shared::auth::SaTokenInterceptor;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};
use toolset_proto::toolset_controller_server::{ToolsetController, ToolsetControllerServer};
use toolset_proto::*;

/// Fake controller whose only real method is `get_tool_call`: it runs the
/// production `extract_bearer_token` on the request and records the token it
/// saw. Every other RPC is unreachable in this test.
struct FakeController {
    seen_token: Arc<Mutex<Option<String>>>,
}

type EventStream = Pin<Box<dyn Stream<Item = Result<TurnEvent, Status>> + Send>>;
type ToolListStream = Pin<Box<dyn Stream<Item = Result<ToolListUpdate, Status>> + Send>>;
type FrameStream = Pin<Box<dyn Stream<Item = Result<ToolResultFrame, Status>> + Send>>;

#[tonic::async_trait]
impl ToolsetController for FakeController {
    type TurnStream = EventStream;
    type WatchToolsStream = ToolListStream;
    type AwaitToolResultStream = FrameStream;

    async fn get_tool_call(
        &self,
        request: Request<GetToolCallRequest>,
    ) -> Result<Response<ToolCallAssignment>, Status> {
        let token = shared::auth::extract_bearer_token(&request)?.to_string();
        *self.seen_token.lock().unwrap() = Some(token);
        Ok(Response::new(ToolCallAssignment {
            call_id: "test-call".to_string(),
            working_dir: String::new(),
            args: Default::default(),
        }))
    }

    async fn turn(&self, _: Request<TurnRequest>) -> Result<Response<Self::TurnStream>, Status> {
        unimplemented!()
    }
    async fn cancel_turn(
        &self,
        _: Request<CancelTurnRequest>,
    ) -> Result<Response<CancelTurnResponse>, Status> {
        unimplemented!()
    }
    async fn watch_tools(
        &self,
        _: Request<WatchToolsRequest>,
    ) -> Result<Response<Self::WatchToolsStream>, Status> {
        unimplemented!()
    }
    async fn begin_tool_call(
        &self,
        _: Request<CallToolRequest>,
    ) -> Result<Response<ToolCallHandle>, Status> {
        unimplemented!()
    }
    async fn await_tool_result(
        &self,
        _: Request<AwaitToolResultRequest>,
    ) -> Result<Response<Self::AwaitToolResultStream>, Status> {
        unimplemented!()
    }
    async fn cancel_tool_call(
        &self,
        _: Request<CancelToolCallRequest>,
    ) -> Result<Response<CancelToolCallResponse>, Status> {
        unimplemented!()
    }
    async fn get_turn(
        &self,
        _: Request<GetTurnRequest>,
    ) -> Result<Response<TurnAssignment>, Status> {
        unimplemented!()
    }
    async fn stream_turn_result(
        &self,
        _: Request<tonic::Streaming<TurnResultChunk>>,
    ) -> Result<Response<TurnAck>, Status> {
        unimplemented!()
    }
    async fn await_turn_cancel(
        &self,
        _: Request<AwaitTurnCancelRequest>,
    ) -> Result<Response<TurnCancelSignal>, Status> {
        unimplemented!()
    }
    async fn stream_tool_result(
        &self,
        _: Request<tonic::Streaming<ToolResultFrame>>,
    ) -> Result<Response<SendToolResultAck>, Status> {
        unimplemented!()
    }
    async fn await_tool_cancel(
        &self,
        _: Request<AwaitToolCancelRequest>,
    ) -> Result<Response<ToolCancelSignal>, Status> {
        unimplemented!()
    }
    async fn report_discovered_tools(
        &self,
        _: Request<ReportDiscoveredToolsRequest>,
    ) -> Result<Response<ReportDiscoveredToolsAck>, Status> {
        unimplemented!()
    }
}

#[tokio::test]
async fn tool_job_client_attaches_sa_token_bearer_header() {
    // Reserve an ephemeral port, then hand it to the server. connect's retry
    // loop tolerates the reserve→serve gap.
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reserve.local_addr().unwrap();
    drop(reserve);

    let seen_token = Arc::new(Mutex::new(None));
    let svc = FakeController {
        seen_token: seen_token.clone(),
    };
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(ToolsetControllerServer::new(svc))
            .serve(addr)
            .await
            .unwrap();
    });

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token");
    std::fs::write(&token_path, "tool-job-sa-token\n").unwrap();

    let endpoint = format!("http://{addr}");
    let mut client =
        toolset_runtime::connect_authenticated(&endpoint, SaTokenInterceptor::new(&token_path))
            .await
            .expect("connect");

    let resp = client
        .get_tool_call(GetToolCallRequest {
            job_id: "job".to_string(),
            tool_name: "Search".to_string(),
        })
        .await
        .expect("authenticated call must succeed");

    assert_eq!(resp.into_inner().call_id, "test-call");
    assert_eq!(
        seen_token.lock().unwrap().as_deref(),
        Some("tool-job-sa-token"),
        "the server must observe the trimmed SA token as a Bearer header"
    );
}
