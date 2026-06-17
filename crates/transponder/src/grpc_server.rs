//! Transponder's inbound gRPC server. Hosts a small `TransponderControl`
//! service for tightbeam-controller to forward external client tool
//! calls to. The transponder is the per-workspace tool catalog
//! authority; this surface lets tightbeam reuse that authority without
//! growing its own SA-token audiences for airlock + mainframe.
//!
//! Wire protocol: `tightbeam-proto::TransponderControl` (WatchTools,
//! CallTool — identical shapes to airlock/mainframe). Auth: SA token,
//! audience `tightbeam.transponder.sycophant.md`, verified via
//! TokenReview.

use std::sync::Arc;

use tightbeam_proto::transponder_control_server::TransponderControl;
use tightbeam_proto::{
    CallToolRequest, CallToolResponse, ToolInfo, ToolListUpdate, WatchToolsRequest,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::clients::TightbeamClient;
use crate::tool_router::ToolRouter;

/// Service impl. Cloning is cheap (Arc-shared router, cheap-clone tightbeam).
#[derive(Clone)]
pub(crate) struct TransponderService {
    router: Arc<ToolRouter>,
    tightbeam: TightbeamClient,
}

impl TransponderService {
    pub(crate) fn new(router: Arc<ToolRouter>, tightbeam: TightbeamClient) -> Self {
        Self { router, tightbeam }
    }
}

#[tonic::async_trait]
impl TransponderControl for TransponderService {
    type WatchToolsStream = ReceiverStream<Result<ToolListUpdate, Status>>;

    async fn watch_tools(
        &self,
        _request: Request<WatchToolsRequest>,
    ) -> Result<Response<Self::WatchToolsStream>, Status> {
        // Snapshot-and-idle for v1. Future: subscribe to a broadcast on
        // ToolRouter that fires on every apply_*_tools.
        let tools_proto = self.router.tool_definitions();
        let tools = tools_proto
            .into_iter()
            .map(|t| ToolInfo {
                name: t.name,
                description: t.description,
                parameters_json: t.parameters_json,
            })
            .collect();
        let (tx, rx) = mpsc::channel(4);
        let _ = tx.send(Ok(ToolListUpdate { tools })).await;
        // Hold the channel open until the client disconnects — dropping
        // tx here would EOF the stream immediately. Park it in a
        // background task; when the receiver disconnects, the next send
        // (which will never come) would fail, but the task ends when
        // the runtime drops it.
        tokio::spawn(async move {
            let _keep = tx;
            // Park forever via a never-resolving sleep.
            tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24 * 365)).await;
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn call_tool(
        &self,
        request: Request<CallToolRequest>,
    ) -> Result<Response<CallToolResponse>, Status> {
        let req = request.into_inner();
        let mut tightbeam = self.tightbeam.clone();
        // Client-driven CallTool has no reply_channel context — these
        // calls do not originate from a chat turn. Channel-source tools
        // would fail with "no reply_channel" which is the correct
        // behaviour: the user can't trigger RevealPath at themselves.
        match self
            .router
            .call_tool(
                &req.name,
                &req.input_json,
                &mut tightbeam,
                /* conversation_id */ "",
                /* reply_channel */ None,
                /* tool_call_id */ "",
            )
            .await
        {
            Ok(resp) => Ok(Response::new(CallToolResponse {
                output: resp.output,
                is_error: resp.is_error,
            })),
            Err(e) => Ok(Response::new(CallToolResponse {
                output: format!("call_tool error: {e}"),
                is_error: true,
            })),
        }
    }
}
