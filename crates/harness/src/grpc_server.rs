//! Harness's inbound gRPC server. Hosts a small `HarnessControl`
//! service for relay-controller to forward external client tool
//! calls to. The harness is the per-workspace tool catalog
//! authority; this surface lets relay reuse that authority without
//! growing its own SA-token audience for toolset.
//!
//! Wire protocol: `toolset-proto::HarnessControl` (WatchTools,
//! CallTool — identical shapes to the toolset controller). Auth: SA token,
//! audience `relay.harness.sycophant.md`, verified via
//! TokenReview.

use std::sync::Arc;

use proto_common::{
    AwaitToolResultRequest, CallToolRequest, CancelToolRequest, CancelToolResponse,
    CancelTurnRequest, CancelTurnResponse, ConversationSummary, DeleteConversationRequest,
    DeleteConversationResponse, DispatchToolResponse, GetConversationHistoryRequest,
    GetConversationHistoryResponse, HistoryEntry, ListConversationsRequest,
    ListConversationsResponse, MintConversationRequest, MintConversationResponse,
    SetConversationNameRequest, SetConversationNameResponse, ToolInfo, ToolListUpdate,
    ToolResultFrame, WatchToolsRequest,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use toolset_proto::harness_control_server::HarnessControl;

use crate::clients::{ToolsetClient, ToolsetRpc};
use crate::conversation::MAX_CONVERSATION_NAME_CHARS;
use crate::registry::ConversationRegistry;
use crate::tool_router::ToolRouter;

/// Upper bound on `GetConversationHistory.limit`; larger requests are
/// clamped so one call can't materialize an unbounded log tail.
const MAX_HISTORY_LIMIT: usize = 500;

/// Clamp a requested history limit. `None`/`Some(0)` → no limit (full log);
/// positive values are capped at [`MAX_HISTORY_LIMIT`].
fn effective_history_limit(requested: Option<u32>) -> Option<usize> {
    match requested {
        None | Some(0) => None,
        Some(n) => Some((n as usize).min(MAX_HISTORY_LIMIT)),
    }
}

/// Service impl. Cloning is cheap (Arc-shared router + registry). Generic over
/// the toolset RPC type `A` (defaulting to the production `ToolsetClient`) purely
/// as a test seam: it lets a client-facing test back the router's `Source::Toolset`
/// arm with a `FakeToolset` and assert the dispatch/await/cancel behavior this
/// service returns to the client — production wiring is unchanged by the default.
#[derive(Clone)]
pub(crate) struct HarnessService<A = ToolsetClient> {
    router: Arc<ToolRouter<A>>,
    registry: Arc<ConversationRegistry>,
}

impl<A> HarnessService<A> {
    pub(crate) fn new(router: Arc<ToolRouter<A>>, registry: Arc<ConversationRegistry>) -> Self {
        Self { router, registry }
    }
}

#[tonic::async_trait]
impl<A: ToolsetRpc + Clone + Send + Sync + 'static> HarnessControl for HarnessService<A> {
    type WatchToolsStream = ReceiverStream<Result<ToolListUpdate, Status>>;
    type AwaitToolResultStream = ReceiverStream<Result<ToolResultFrame, Status>>;

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
                toolset: String::new(),
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

    async fn dispatch_tool(
        &self,
        request: Request<CallToolRequest>,
    ) -> Result<Response<DispatchToolResponse>, Status> {
        let req = request.into_inner();
        // conversation_id is optional: an app-run dispatch with no conversation
        // (e.g. the browser pane's file listing, which runs before any chat is
        // selected) streams live but persists no per-conversation execution.json.
        // When a conversation IS named it must be owned, so its frames land in
        // that conversation's execution.json and not another's.
        if !req.conversation_id.is_empty() && !self.registry.owns(&req.conversation_id).await {
            return Err(Status::not_found("conversation_id not found"));
        }
        // Mint the toolset call_id, register the session, and return the id
        // before the call resolves — the consumer runs in the background,
        // appending frames and staying subscribed through a cancel.
        let call_id = self
            .router
            .dispatch_client_tool(&req.name, &req.input_json, &req.conversation_id)
            .await
            .map_err(|e| Status::internal(format!("dispatch_tool: {e}")))?;
        Ok(Response::new(DispatchToolResponse { call_id }))
    }

    async fn await_tool_result(
        &self,
        request: Request<AwaitToolResultRequest>,
    ) -> Result<Response<Self::AwaitToolResultStream>, Status> {
        let req = request.into_inner();
        if req.call_id.is_empty() {
            return Err(Status::invalid_argument(
                "AwaitToolResultRequest.call_id required",
            ));
        }
        let stream = self
            .router
            .await_client_tool(&req.call_id, &req.conversation_id)
            .await?;
        Ok(Response::new(stream))
    }

    async fn cancel_tool(
        &self,
        request: Request<CancelToolRequest>,
    ) -> Result<Response<CancelToolResponse>, Status> {
        let req = request.into_inner();
        if req.call_id.is_empty() {
            return Err(Status::invalid_argument(
                "CancelToolRequest.call_id required",
            ));
        }
        // Forward to the toolset only for an in-flight call; an unknown or
        // already-retired id is answered here and reports nothing canceled.
        let cancelled = self.router.cancel_client_tool(&req.call_id).await;
        Ok(Response::new(CancelToolResponse { cancelled }))
    }

    async fn mint_conversation(
        &self,
        request: Request<MintConversationRequest>,
    ) -> Result<Response<MintConversationResponse>, Status> {
        let req = request.into_inner();
        let conversation_id = self
            .registry
            .mint(&req.owner)
            .await
            .map_err(|e| Status::internal(format!("failed to mint conversation: {e}")))?;
        Ok(Response::new(MintConversationResponse { conversation_id }))
    }

    async fn list_conversations(
        &self,
        request: Request<ListConversationsRequest>,
    ) -> Result<Response<ListConversationsResponse>, Status> {
        // Single-workspace: every conversation in the registry belongs to
        // this harness's workspace, so the listing is narrowed by the
        // caller's opaque owner key instead.
        let req = request.into_inner();
        let owner = req.owner;
        let conversations = self
            .registry
            .list_summaries(&owner)
            .await
            .into_iter()
            .map(|(id, ts, name)| ConversationSummary {
                conversation_id: id,
                last_touched_ms_epoch: ts,
                name,
                owner: owner.clone(),
            })
            .collect();
        Ok(Response::new(ListConversationsResponse { conversations }))
    }

    async fn delete_conversation(
        &self,
        request: Request<DeleteConversationRequest>,
    ) -> Result<Response<DeleteConversationResponse>, Status> {
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "DeleteConversationRequest.conversation_id required",
            ));
        }
        if !self.registry.owns(&req.conversation_id).await {
            return Err(Status::not_found("conversation_id not found"));
        }
        self.registry
            .delete(&req.conversation_id)
            .await
            .map_err(|e| Status::internal(format!("failed to delete conversation: {e}")))?;
        Ok(Response::new(DeleteConversationResponse {}))
    }

    async fn set_conversation_name(
        &self,
        request: Request<SetConversationNameRequest>,
    ) -> Result<Response<SetConversationNameResponse>, Status> {
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "SetConversationNameRequest.conversation_id required",
            ));
        }
        if req.name.chars().count() > MAX_CONVERSATION_NAME_CHARS {
            return Err(Status::invalid_argument(format!(
                "name exceeds {MAX_CONVERSATION_NAME_CHARS}-character limit"
            )));
        }
        if !self.registry.owns(&req.conversation_id).await {
            return Err(Status::not_found("conversation_id not found"));
        }
        self.registry
            .set_name(&req.conversation_id, &req.name)
            .await
            .map_err(|e| Status::internal(format!("failed to persist conversation name: {e}")))?;
        Ok(Response::new(SetConversationNameResponse {}))
    }

    async fn cancel_turn(
        &self,
        request: Request<CancelTurnRequest>,
    ) -> Result<Response<CancelTurnResponse>, Status> {
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "CancelTurnRequest.conversation_id required",
            ));
        }
        if !self.registry.owns(&req.conversation_id).await {
            return Err(Status::not_found("conversation_id not found"));
        }
        // Local stop: fire the in-flight turn's token (if any). No cascade
        // into running toolsets/subagents.
        let cancelled = self.registry.cancel(&req.conversation_id).await;
        Ok(Response::new(CancelTurnResponse { cancelled }))
    }

    async fn get_conversation_history(
        &self,
        request: Request<GetConversationHistoryRequest>,
    ) -> Result<Response<GetConversationHistoryResponse>, Status> {
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument("conversation_id required"));
        }
        if !self
            .registry
            .owned_by(&req.conversation_id, &req.owner)
            .await
        {
            return Err(Status::not_found("conversation_id not found"));
        }
        let limit = effective_history_limit(req.limit);
        let log = self
            .registry
            .get_or_create(&req.conversation_id)
            .await
            .map_err(|e| Status::internal(format!("load conversation: {e}")))?;
        let snap = log.read().await.snapshot(limit);
        let truncated = (snap.entries.len() as u64) < snap.total_seq;
        let entries = snap
            .entries
            .into_iter()
            .map(|e| HistoryEntry {
                seq: e.seq,
                ts: e.ts,
                message: Some(e.message),
                tag: e.tag,
            })
            .collect();
        Ok(Response::new(GetConversationHistoryResponse {
            entries,
            total_seq: snap.total_seq,
            truncated,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
    use crate::test_doubles::{test_kernel, TEST_WS};

    /// A service whose registry owns nothing: fresh tempdir-backed factory,
    /// no conversations minted.
    fn service_owning_nothing() -> HarnessService {
        let root = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(root));
        let registry = Arc::new(ConversationRegistry::new(factory));
        let router = Arc::new(ToolRouter::new(
            test_kernel(),
            TEST_WS.to_string(),
            None,
            None,
            registry.clone(),
        ));
        HarnessService::new(router, registry)
    }

    // EARS: "Where the CancelTurn's conversation_id is not owned by this
    // harness, the harness shall reject the request with NotFound."
    // Materiality: negate or short-circuit the line-224 ownership guard
    // (`if !self.registry.owns(...)` -> `if false` / drop the `!`) and the
    // handler falls through to registry.cancel on an unowned id, returning
    // Ok(cancelled=false) instead of the NotFound reject this test demands.
    #[tokio::test]
    async fn cancel_turn_on_unowned_conversation_returns_not_found() {
        let svc = service_owning_nothing();
        let result = svc
            .cancel_turn(Request::new(CancelTurnRequest {
                conversation_id: "never-minted".to_string(),
            }))
            .await;
        let status = result.expect_err("unowned conversation_id must be rejected");
        assert_eq!(status.code(), tonic::Code::NotFound);
    }
}

// Tests for the client-facing dispatch / await / cancel split, driving the real
// `HarnessControl` handlers with a `FakeToolset` — no live gRPC server.
#[cfg(test)]
mod dispatch_await_cancel_tests {
    use super::*;
    use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
    use crate::execution_log::{ExecutionLogWriter, LocalFsExecutionLog};
    use crate::test_doubles::FakeToolset;
    use proto_common::tool_result_frame::Frame;
    use proto_common::{
        AwaitToolResultRequest, CancelToolRequest, ToolComplete, ToolOutcome, ToolResultFrame,
    };
    use std::time::Duration;
    use tokio_stream::StreamExt;

    fn stdout_f(s: &str) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Stdout(s.into())),
        }
    }
    fn done_terminal() -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Complete(ToolComplete {
                outcome: ToolOutcome::Done as i32,
                exit_code: 0,
            })),
        }
    }
    fn canceled_terminal() -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Complete(ToolComplete {
                outcome: ToolOutcome::Canceled as i32,
                exit_code: -1,
            })),
        }
    }
    fn is_complete(frame: &ToolResultFrame) -> bool {
        matches!(frame.frame.as_ref(), Some(Frame::Complete(_)))
    }

    /// Build the service backed by the given `FakeToolset`, an empty
    /// conversation registry, and a temp-dir execution log (so the await path
    /// has a persisted store to replay from).
    fn service_with(toolset: FakeToolset) -> HarnessService<FakeToolset> {
        let root = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(root));
        let registry = Arc::new(ConversationRegistry::new(factory));
        let exec_dir = tempfile::TempDir::new().unwrap().keep();
        let exec_log: Arc<dyn ExecutionLogWriter> =
            Arc::new(LocalFsExecutionLog::new(exec_dir, "test-conv".to_string()));
        let router: Arc<ToolRouter<FakeToolset>> = Arc::new(
            ToolRouter::new(
                crate::test_doubles::test_kernel(),
                crate::test_doubles::TEST_WS.to_string(),
                Some(toolset),
                None,
                registry.clone(),
            )
            .with_execution_log(exec_log),
        );
        router
            .apply_toolset_tools(vec![proto_common::ToolInfo {
                toolset: "stdlib".into(),
                name: "Bash".into(),
                description: "run a shell tool".into(),
                parameters_json: "{}".into(),
            }])
            .unwrap();
        HarnessService::new(router, registry)
    }

    async fn dispatch(svc: &HarnessService<FakeToolset>) -> String {
        let conversation_id = svc
            .mint_conversation(Request::new(MintConversationRequest {
                owner: "test-owner".into(),
            }))
            .await
            .expect("mint a conversation to attach the dispatch to")
            .into_inner()
            .conversation_id;
        svc.dispatch_tool(Request::new(CallToolRequest {
            name: "Bash".into(),
            input_json: "{}".into(),
            conversation_id,
        }))
        .await
        .expect("dispatch returns Ok")
        .into_inner()
        .call_id
    }

    async fn collect_await(
        svc: &HarnessService<FakeToolset>,
        call_id: &str,
        conversation_id: &str,
    ) -> Vec<ToolResultFrame> {
        let stream = svc
            .await_tool_result(Request::new(AwaitToolResultRequest {
                call_id: call_id.to_string(),
                conversation_id: conversation_id.to_string(),
            }))
            .await
            .expect("await returns a frame stream")
            .into_inner();
        let mut stream = std::pin::pin!(stream);
        let mut frames = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
                Ok(Some(Ok(f))) => {
                    let done = is_complete(&f);
                    frames.push(f);
                    if done {
                        break;
                    }
                }
                Ok(Some(Err(e))) => panic!("await stream errored: {e}"),
                Ok(None) => break,
                Err(_) => panic!(
                    "await stream stalled before the terminal, collected {} frames",
                    frames.len()
                ),
            }
        }
        frames
    }

    // Dispatch surfaces the toolset's server-minted call_id to the client
    // BEFORE the call resolves. The FakeToolset's frame stream pends forever
    // (`None`), so the only way dispatch can return is by NOT waiting on the
    // result — it just mints and returns the id.
    //
    // Materiality: a dispatch that consumes the frame stream to completion before
    // returning hangs against a never-terminating stream until the outer timeout
    // fires. Surfacing the wrong id fails the equality.
    #[tokio::test]
    async fn dispatch_surfaces_the_minted_call_id_before_the_result() {
        let toolset = FakeToolset::new("call-mint-1", None);
        let svc = service_with(toolset);
        let conversation_id = svc
            .mint_conversation(Request::new(MintConversationRequest {
                owner: "test-owner".into(),
            }))
            .await
            .expect("mint a conversation to attach the dispatch to")
            .into_inner()
            .conversation_id;
        let resp = tokio::time::timeout(
            Duration::from_secs(3),
            svc.dispatch_tool(Request::new(CallToolRequest {
                name: "Bash".into(),
                input_json: "{}".into(),
                conversation_id,
            })),
        )
        .await
        .expect("dispatch must return the call_id without awaiting the result")
        .expect("dispatch returns Ok")
        .into_inner();
        assert_eq!(
            resp.call_id, "call-mint-1",
            "dispatch surfaces the toolset's server-minted call_id"
        );
    }

    // A CancelTool for an in-flight call forwards the call_id to the
    // toolset — the forward is what fires the registered cancel token that the
    // toolset runtime long-polls and answers by killing its own child (rather
    // than letting it run to completion). The FakeToolset's stream pends, so the
    // call is still in-flight when the cancel arrives.
    //
    // Materiality: the harness dropping the session / not forwarding the
    // cancel (regressing to fire-and-forget-then-drop, or answering without
    // calling cancel_tool_call) leaves `cancels()` empty — the runtime never
    // learns to kill — and this test reds.
    #[tokio::test]
    async fn cancel_of_an_in_flight_call_forwards_to_the_toolset() {
        let toolset = FakeToolset::new("call-live-1", None);
        let svc = service_with(toolset.clone());
        let call_id = dispatch(&svc).await;
        assert_eq!(call_id, "call-live-1");

        let resp = svc
            .cancel_tool(Request::new(CancelToolRequest {
                call_id: "call-live-1".into(),
            }))
            .await
            .expect("cancel returns Ok")
            .into_inner();
        assert!(
            resp.cancelled,
            "canceling an in-flight call reports it canceled"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            toolset.cancels().contains(&"call-live-1".to_string()),
            "the cancel is forwarded to the toolset so the runtime kills its child, got {:?}",
            toolset.cancels()
        );
    }

    // A CancelTool naming a call_id with no in-flight call reports that no
    // call was canceled — and does NOT forward to the toolset (there is nothing
    // to cancel). The FakeToolset would answer `true` to any forwarded cancel,
    // so a forwarding mistake is observable.
    //
    // Materiality: answering `true` for an unknown id (e.g. forwarding
    // unconditionally, or returning a hardcoded true) reds the `!cancelled`
    // assertion; a forward for the unknown id reds the empty-`cancels()`
    // assertion.
    #[tokio::test]
    async fn cancel_of_an_unknown_call_id_reports_none_canceled() {
        let toolset = FakeToolset::new("call-unrelated", Some(vec![]));
        let svc = service_with(toolset.clone());
        let resp = svc
            .cancel_tool(Request::new(CancelToolRequest {
                call_id: "never-dispatched".into(),
            }))
            .await
            .expect("cancel returns Ok")
            .into_inner();
        assert!(
            !resp.cancelled,
            "canceling a call_id with no in-flight call reports that no call was canceled"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            toolset.cancels().is_empty(),
            "an unknown call_id is answered by the harness itself, not forwarded, got {:?}",
            toolset.cancels()
        );
    }

    // While a call is pending the client's await surface serves the call's
    // output as a live stream of individual frames — not a single collapsed
    // response. The scripted stream carries a non-terminal stdout frame ahead of
    // the terminal; the client must see that stdout frame on the wire.
    //
    // Materiality: an await that collapses the frames into one assembled
    // response (the old unary behavior) drops the non-terminal stdout frame, so
    // the "live-chunk-alpha" assertion reds. Ending without a terminal reds the
    // terminal assertion.
    #[tokio::test]
    async fn await_streams_individual_frames_not_a_collapsed_response() {
        let toolset = FakeToolset::new(
            "call-stream-1",
            Some(vec![stdout_f("live-chunk-alpha"), done_terminal()]),
        );
        let svc = service_with(toolset);
        dispatch(&svc).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let frames = collect_await(&svc, "call-stream-1", "").await;
        assert!(
            frames
                .iter()
                .any(|f| matches!(f.frame.as_ref(), Some(Frame::Stdout(s)) if s == "live-chunk-alpha")),
            "await streams the individual stdout frame, not just a collapsed terminal, got {} frames",
            frames.len()
        );
        assert!(
            frames.last().map(is_complete).unwrap_or(false),
            "the await stream ends on the terminal frame"
        );
    }

    // The live in-flight follow path: while a call is GENUINELY still in
    // flight, a client that subscribes mid-stream receives the terminal frame
    // through the live fan-out — not just the frames already buffered at
    // subscribe time. The gated FakeToolset parks its consumer on the gate just
    // before the terminal, so at subscribe the call is still present in the
    // router's session map and its snapshot holds no terminal; the terminal is
    // released (via the gate) only AFTER the client has subscribed, so it can
    // reach the client only through the in-flight follow-loop, never the
    // persisted-fallback path the sibling await tests land on.
    //
    // Materiality: inverting the `if !terminal_seen` guard skips the follow-loop
    // for a live call whose snapshot carries no terminal, so the gate-released
    // terminal never
    // reaches the client — the stream ends on the stdout frame and the terminal
    // assertion reds.
    #[tokio::test]
    async fn await_follows_a_live_call_to_its_terminal_via_the_fan_out() {
        use std::sync::Arc;
        use tokio::sync::Notify;

        let gate = Arc::new(Notify::new());
        let toolset = FakeToolset::new(
            "call-live-follow",
            Some(vec![stdout_f("pre-subscribe-chunk"), done_terminal()]),
        )
        .with_gate(gate.clone());
        let svc = service_with(toolset);

        let call_id = dispatch(&svc).await;
        assert_eq!(call_id, "call-live-follow");

        // Let the consumer drain the stdout frame and park on the gate before the
        // terminal, so the call is still live and its snapshot holds no terminal.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Subscribe while the call is genuinely in flight. `await_client_tool`
        // snapshots the frames and subscribes to the fan-out synchronously before
        // returning, so releasing the gate now forces the terminal down the
        // live-follow loop, not the snapshot.
        let stream = svc
            .await_tool_result(Request::new(AwaitToolResultRequest {
                call_id: "call-live-follow".into(),
                conversation_id: String::new(),
            }))
            .await
            .expect("await returns a frame stream")
            .into_inner();
        gate.notify_one();

        let mut stream = std::pin::pin!(stream);
        let mut frames = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
                Ok(Some(Ok(f))) => {
                    let done = is_complete(&f);
                    frames.push(f);
                    if done {
                        break;
                    }
                }
                Ok(Some(Err(e))) => panic!("await stream errored: {e}"),
                Ok(None) => break,
                Err(_) => panic!(
                    "await stalled before the terminal, collected {} frames",
                    frames.len()
                ),
            }
        }

        assert!(
            frames.iter().any(
                |f| matches!(f.frame.as_ref(), Some(Frame::Stdout(s)) if s == "pre-subscribe-chunk")
            ),
            "the client receives the in-flight call's frames as a live stream, got {} frames",
            frames.len()
        );
        let last = frames
            .last()
            .expect("the live-follow terminal must reach the client");
        match last.frame.as_ref() {
            Some(Frame::Complete(c)) => assert_eq!(
                c.outcome(),
                ToolOutcome::Done,
                "the DONE terminal emitted after subscribe reaches the client through the live in-flight follow-loop, got {:?}",
                c.outcome()
            ),
            other => panic!("the await stream must end on the scripted DONE terminal, got {other:?}"),
        }
    }

    // A canceled call's await stream ends on a terminal that indicates
    // CANCELED — distinct from the done and failed terminals. The runtime-emitted
    // canceled outcome reaches the client unchanged.
    //
    // Materiality: the harness reclassifying the canceled terminal (folding
    // it into a generic error/failed, or coercing to done) reds the
    // `outcome() == CANCELED` assertion — the exact distinction the client must
    // be able to draw.
    #[tokio::test]
    async fn canceled_call_await_ends_on_a_canceled_terminal() {
        let toolset = FakeToolset::new(
            "call-cancel-1",
            Some(vec![stdout_f("partial-before-cancel"), canceled_terminal()]),
        );
        let svc = service_with(toolset);
        dispatch(&svc).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let frames = collect_await(&svc, "call-cancel-1", "").await;
        let last = frames
            .last()
            .expect("the stream must carry a terminal frame");
        match last.frame.as_ref() {
            Some(Frame::Complete(c)) => assert_eq!(
                c.outcome(),
                ToolOutcome::Canceled,
                "the canceled terminal reaches the client as CANCELED, distinct from done/failed"
            ),
            other => panic!("the last frame must be the terminal ToolComplete, got {other:?}"),
        }
    }

    // A client-driven call whose toolset frame stream ends abnormally — one or
    // two frames, then a frame-stream error with no terminal ToolComplete —
    // must still terminate the client's await stream in a terminal outcome. The
    // session consumer breaks on the frame-stream error and retires the session
    // without emitting a terminal; an awaiter following the live fan-out then
    // sees the sender drop, ends its follow-loop, and (without a fix) closes the
    // client stream with no terminal at all, leaving the client hanging. Every
    // client-driven call must end in one of the three outcomes; a synthetic
    // FAILED terminal is owed when the stream dies mid-call.
    //
    // Materiality: the production change that makes this pass is the synthetic-
    // failed-terminal emission in `await_client_tool` — when the live follow-loop
    // ends with no terminal seen, emit a `ToolComplete { outcome: FAILED }`.
    // Remove that emission and the awaited stream's last frame is the stdout
    // chunk (the stream just closes), not a terminal — reding the assertion
    // below. The gated abnormal stream keeps the call genuinely in flight at
    // subscribe, so the terminalless close is reached via the in-flight follow
    // branch, not the persisted-fallback path.
    #[tokio::test]
    async fn await_of_an_abnormally_ended_live_call_ends_on_a_failed_terminal() {
        use std::sync::Arc;
        use tokio::sync::Notify;

        let gate = Arc::new(Notify::new());
        // Yield one stdout frame, park on the gate (call stays live), then error
        // with no terminal frame once the gate releases.
        let toolset = FakeToolset::new(
            "call-abnormal-end",
            Some(vec![stdout_f("partial-then-death")]),
        )
        .with_gate(gate.clone())
        .erring_after_gate("toolset frame stream broke");
        let svc = service_with(toolset);

        let call_id = dispatch(&svc).await;
        assert_eq!(call_id, "call-abnormal-end");

        // Let the consumer drain the stdout frame and park on the gate, so the
        // call is still live and its snapshot holds no terminal.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Subscribe while the call is genuinely in flight — the live in-flight
        // follow branch, not the persisted-fallback path.
        let stream = svc
            .await_tool_result(Request::new(AwaitToolResultRequest {
                call_id: "call-abnormal-end".into(),
                conversation_id: String::new(),
            }))
            .await
            .expect("await returns a frame stream")
            .into_inner();

        // Release the gate: the consumer now returns the frame-stream error,
        // breaks, and retires the session without a terminal.
        gate.notify_one();

        let mut stream = std::pin::pin!(stream);
        let mut frames = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
                Ok(Some(Ok(f))) => {
                    let done = is_complete(&f);
                    frames.push(f);
                    if done {
                        break;
                    }
                }
                Ok(Some(Err(e))) => panic!("await stream errored: {e}"),
                Ok(None) => break,
                Err(_) => panic!(
                    "await stalled before any terminal, collected {} frames",
                    frames.len()
                ),
            }
        }

        let last = frames
            .last()
            .expect("an abnormally ended call must still yield a terminal frame to the client");
        match last.frame.as_ref() {
            Some(Frame::Complete(c)) => {
                assert_eq!(
                    c.outcome(),
                    ToolOutcome::Failed,
                    "an abnormal stream end must terminate the client's await on a synthetic FAILED terminal, got {:?}",
                    c.outcome()
                );
                assert_eq!(
                    c.exit_code, -1,
                    "the synthetic FAILED terminal carries the sentinel exit_code -1, got {}",
                    c.exit_code
                );
            }
            other => panic!("the await stream must end on a terminal ToolComplete, got {other:?}"),
        }
    }

    // The same terminalless-close hole on the persisted-fallback replay path: a
    // call whose stream EOFs with no terminal retires the session leaving a
    // truncated persisted record (frames, no terminal). A later awaiter — served
    // from that record, not the live fan-out — must still end on a terminal.
    //
    // Materiality: the production change that makes this pass is the synthetic-
    // failed-terminal emission in `await_client_tool`'s persisted-replay branch —
    // when the replayed record has no terminal (`has_terminal()` is false), emit
    // a `ToolComplete { outcome: FAILED }` after the frames. Remove that emission
    // and the replayed stream's last frame is the stdout chunk, not a terminal —
    // reding the assertion below.
    #[tokio::test]
    async fn await_of_a_truncated_persisted_record_ends_on_a_failed_terminal() {
        // Yield one stdout frame then EOF with no terminal, so the session
        // retires leaving a truncated persisted record.
        let toolset = FakeToolset::new(
            "call-truncated-replay",
            Some(vec![stdout_f("stdout-before-truncation")]),
        );
        let svc = service_with(toolset);

        dispatch(&svc).await;
        // Let the consumer drain, hit EOF, and retire the session so the await
        // is served from the persisted record, not the live fan-out.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let frames = collect_await(&svc, "call-truncated-replay", "").await;
        let last = frames
            .last()
            .expect("a truncated persisted record must still yield a terminal frame to the client");
        match last.frame.as_ref() {
            Some(Frame::Complete(c)) => {
                assert_eq!(
                    c.outcome(),
                    ToolOutcome::Failed,
                    "a truncated persisted record must terminate the client's await on a synthetic FAILED terminal, got {:?}",
                    c.outcome()
                );
                assert_eq!(
                    c.exit_code, -1,
                    "the synthetic FAILED terminal carries the sentinel exit_code -1, got {}",
                    c.exit_code
                );
            }
            other => panic!("the await stream must end on a terminal ToolComplete, got {other:?}"),
        }
    }

    /// Build a service whose registry is rooted at a fresh tempdir, mint one
    /// conversation, and return `(service, root, conv_id)`. The router has no
    /// separate execution-log dir: it derives the per-conversation writer from
    /// the registry's factory root, so conversation `X`'s frames persist at
    /// `<root>/default/<X>/execution.json`.
    async fn service_and_conversation(
        toolset: FakeToolset,
    ) -> (HarnessService<FakeToolset>, std::path::PathBuf, String) {
        let root = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> =
            Arc::new(LocalFsFactory::new(root.clone()));
        let registry = Arc::new(ConversationRegistry::new(factory));
        let conv_id = registry.mint("test-owner").await.unwrap();
        let router: Arc<ToolRouter<FakeToolset>> = Arc::new(ToolRouter::new(
            crate::test_doubles::test_kernel(),
            crate::test_doubles::TEST_WS.to_string(),
            Some(toolset),
            None,
            registry.clone(),
        ));
        router
            .apply_toolset_tools(vec![proto_common::ToolInfo {
                toolset: "stdlib".into(),
                name: "Bash".into(),
                description: "run a shell tool".into(),
                parameters_json: "{}".into(),
            }])
            .unwrap();
        let svc = HarnessService::new(router, registry);
        (svc, root, conv_id)
    }

    // An app-dispatched tool attaches the call to the app's active conversation
    // and appends its frames to that conversation's execution.json; re-subscribe
    // replays them filtered by call_id.
    //
    // Materiality: a mutant that writes app-run frames to a process-global or other
    // directory (ignoring conversation_id) leaves `<root>/default/<conv_id>/
    // execution.json` absent -> the is_file/marker assertions red. A mutant that
    // loses the call_id->conversation_id association breaks the disk replay ->
    // await returns NotFound and `collect_await` reds.
    //
    // The load-bearing observable is the on-disk LOCATION (the frames sit in THIS
    // conversation's dir), which a global-writer implementation cannot satisfy;
    // the replay assertion pins the filtered-from-disk read.
    #[tokio::test]
    async fn app_dispatch_persists_frames_to_its_conversations_execution_json() {
        let toolset = FakeToolset::new(
            "call-app-1",
            Some(vec![stdout_f("app-run-marker"), done_terminal()]),
        );
        let (svc, root, conv_id) = service_and_conversation(toolset).await;

        let call_id = svc
            .dispatch_tool(Request::new(CallToolRequest {
                name: "Bash".into(),
                input_json: "{}".into(),
                conversation_id: conv_id.clone(),
            }))
            .await
            .expect("dispatch returns Ok")
            .into_inner()
            .call_id;
        assert_eq!(call_id, "call-app-1");

        // Let the consumer drain both frames and retire the session so the await is
        // served from the persisted record on disk, not the live fan-out.
        tokio::time::sleep(Duration::from_millis(80)).await;

        let exec_json = root.join("default").join(&conv_id).join("execution.json");
        assert!(
            exec_json.is_file(),
            "app-run frames must persist to the active conversation's execution.json at {}",
            exec_json.display()
        );
        let text = std::fs::read_to_string(&exec_json).unwrap();
        assert!(
            text.contains("app-run-marker"),
            "the call's frames are appended to the conversation's execution.json, got {text:?}"
        );

        let frames = collect_await(&svc, &call_id, &conv_id).await;
        assert!(
            frames.iter().any(
                |f| matches!(f.frame.as_ref(), Some(Frame::Stdout(s)) if s == "app-run-marker")
            ),
            "re-subscribe replays the persisted stdout frame filtered by call_id, got {} frames",
            frames.len()
        );
        assert!(
            frames.last().map(is_complete).unwrap_or(false),
            "the replayed stream ends on the terminal frame"
        );
    }

    // A tool dispatched without a conversation id is accepted: a conversation-less
    // app-run call (the browser pane before any chat is selected) runs and returns
    // its minted call_id; it simply persists no per-conversation execution.json.
    //
    // Materiality: reinstate an empty-conversation_id reject in the harness
    // `dispatch_tool` handler and this call returns an error instead of a call_id,
    // so `expect("...")` panics and this reds.
    #[tokio::test]
    async fn dispatch_accepts_empty_conversation_id() {
        let toolset = FakeToolset::new("call-x", None);
        let (svc, _root, _conv) = service_and_conversation(toolset).await;
        let resp = svc
            .dispatch_tool(Request::new(CallToolRequest {
                name: "Bash".into(),
                input_json: "{}".into(),
                conversation_id: String::new(),
            }))
            .await
            .expect("an empty conversation_id is accepted (no conversation attach)");
        assert_eq!(resp.into_inner().call_id, "call-x");
    }

    // A dispatch naming a conversation this harness does not own is rejected —
    // the app can only attach a call to a real, owned conversation.
    //
    // Materiality: drop the `registry.owns` check in the harness `dispatch_tool`
    // handler and a well-formed-but-unowned conversation_id falls through to
    // `dispatch_client_tool`, returning Ok(call_id) -> `expect_err` panics and this
    // reds. (A bare UUID isolates this from any UUID-shape check.)
    //
    // Pins the ownership guard for app dispatch, not the registry's storage
    // behavior.
    #[tokio::test]
    async fn dispatch_rejects_unowned_conversation_id() {
        let toolset = FakeToolset::new("call-y", None);
        let (svc, _root, _conv) = service_and_conversation(toolset).await;
        // A well-formed UUID that was never minted here: passes empty + UUID-shape
        // checks, but the registry does not own it.
        let unowned = uuid::Uuid::new_v4().to_string();
        let err = svc
            .dispatch_tool(Request::new(CallToolRequest {
                name: "Bash".into(),
                input_json: "{}".into(),
                conversation_id: unowned,
            }))
            .await
            .expect_err("an unowned conversation_id must be rejected");
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    fn user_msg(text: &str) -> proto_common::Message {
        proto_common::Message {
            role: "user".into(),
            content: proto_common::text_content(text),
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        }
    }

    // grpc_server.rs:166 `mint_conversation` — the handler returns a fresh id
    // the registry then owns. Materiality: a mint that returns an empty id reds
    // the non-empty assert; one that does not register (the registry never
    // learns the id) reds the `owns` assert.
    #[tokio::test]
    async fn mint_returns_a_nonempty_owned_id() {
        let (svc, _root, _conv) = service_and_conversation(FakeToolset::new("c", None)).await;
        let id = svc
            .mint_conversation(Request::new(MintConversationRequest {
                owner: "test-owner".into(),
            }))
            .await
            .expect("mint returns Ok")
            .into_inner()
            .conversation_id;
        assert!(!id.is_empty(), "mint returns a non-empty conversation id");
        assert!(
            svc.registry.owns(&id).await,
            "the minted id is registered/owned"
        );
    }

    // grpc_server.rs:205 — deleting an OWNED conversation succeeds. Materiality:
    // drop the `!` on the ownership guard (`if !self.registry.owns` -> `if
    // self.registry.owns`) and an owned id short-circuits to NotFound -> the
    // `expect` reds. Pairs with the unowned-NotFound test to pin both arms.
    #[tokio::test]
    async fn delete_conversation_on_owned_succeeds() {
        let (svc, _root, conv_id) = service_and_conversation(FakeToolset::new("c", None)).await;
        svc.delete_conversation(Request::new(DeleteConversationRequest {
            conversation_id: conv_id.clone(),
        }))
        .await
        .expect("deleting an owned conversation succeeds");
        assert!(
            !svc.registry.owns(&conv_id).await,
            "the conversation is gone from the registry after delete"
        );
    }

    // grpc_server.rs:205 — deleting an UNOWNED conversation is rejected with
    // NotFound. Materiality: negate/short-circuit the ownership guard and an
    // unowned id falls through to `registry.delete` returning Ok -> `expect_err`
    // reds.
    #[tokio::test]
    async fn delete_conversation_on_unowned_returns_not_found() {
        let (svc, _root, _conv) = service_and_conversation(FakeToolset::new("c", None)).await;
        let unowned = uuid::Uuid::new_v4().to_string();
        let err = svc
            .delete_conversation(Request::new(DeleteConversationRequest {
                conversation_id: unowned,
            }))
            .await
            .expect_err("deleting an unowned conversation must be rejected");
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    // grpc_server.rs:225 — a name of exactly MAX_CONVERSATION_NAME_CHARS (200)
    // is accepted; the length guard is `> 200`, strict. Materiality: change the
    // guard to `>= 200` and a 200-char name is rejected -> the `expect` reds.
    #[tokio::test]
    async fn set_name_at_limit_is_accepted() {
        let (svc, _root, conv_id) = service_and_conversation(FakeToolset::new("c", None)).await;
        let name = "a".repeat(MAX_CONVERSATION_NAME_CHARS);
        svc.set_conversation_name(Request::new(SetConversationNameRequest {
            conversation_id: conv_id,
            name,
        }))
        .await
        .expect("a name at the character limit is accepted");
    }

    // grpc_server.rs:225 — a name of MAX_CONVERSATION_NAME_CHARS + 1 (201) is
    // rejected with InvalidArgument. Materiality: drop the length guard (or
    // widen the bound) and the over-limit name is accepted -> `expect_err` reds.
    #[tokio::test]
    async fn set_name_over_limit_is_rejected() {
        let (svc, _root, conv_id) = service_and_conversation(FakeToolset::new("c", None)).await;
        let name = "a".repeat(MAX_CONVERSATION_NAME_CHARS + 1);
        let err = svc
            .set_conversation_name(Request::new(SetConversationNameRequest {
                conversation_id: conv_id,
                name,
            }))
            .await
            .expect_err("a name over the character limit must be rejected");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    // grpc_server.rs:277 `truncated = entries.len() < total_seq` — a limit below
    // the entry count clips the head and reports truncated. Materiality: flip
    // the comparison (or drop the clip) and truncated goes false with 3 entries
    // and a limit of 2 -> the `assert!(resp.truncated)` reds. Also pins
    // effective_history_limit's Some(n) arm: entries.len() must equal the limit.
    #[tokio::test]
    async fn history_over_limit_reports_truncated() {
        let (svc, _root, conv_id) = service_and_conversation(FakeToolset::new("c", None)).await;
        let log = svc.registry.get_or_create(&conv_id).await.unwrap();
        {
            let mut l = log.write().await;
            for n in 0..3 {
                l.append(user_msg(&format!("m{n}"))).await.unwrap();
            }
        }
        let resp = svc
            .get_conversation_history(Request::new(GetConversationHistoryRequest {
                conversation_id: conv_id,
                limit: Some(2),
                owner: "test-owner".into(),
            }))
            .await
            .expect("history returns Ok")
            .into_inner();
        assert!(
            resp.truncated,
            "a limit below the entry count clips the head and reports truncated"
        );
        assert_eq!(
            resp.entries.len(),
            2,
            "the returned tail is exactly the requested limit"
        );
    }

    // grpc_server.rs:277 — a full read (no limit) is NOT truncated. Materiality:
    // widen the comparison to `<=` and a full read of 3 entries with total_seq 3
    // reports truncated -> the `assert!(!resp.truncated)` reds.
    #[tokio::test]
    async fn full_history_reports_not_truncated() {
        let (svc, _root, conv_id) = service_and_conversation(FakeToolset::new("c", None)).await;
        let log = svc.registry.get_or_create(&conv_id).await.unwrap();
        {
            let mut l = log.write().await;
            for n in 0..3 {
                l.append(user_msg(&format!("m{n}"))).await.unwrap();
            }
        }
        let resp = svc
            .get_conversation_history(Request::new(GetConversationHistoryRequest {
                conversation_id: conv_id,
                limit: None,
                owner: "test-owner".into(),
            }))
            .await
            .expect("history returns Ok")
            .into_inner();
        assert!(!resp.truncated, "a full read is not truncated");
        assert_eq!(resp.entries.len(), 3);
    }
}
