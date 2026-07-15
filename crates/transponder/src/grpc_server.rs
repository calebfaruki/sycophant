//! Transponder's inbound gRPC server. Hosts a small `TransponderControl`
//! service for hangar-controller to forward external client tool
//! calls to. The transponder is the per-workspace tool catalog
//! authority; this surface lets hangar reuse that authority without
//! growing its own SA-token audiences for airlock + mainframe.
//!
//! Wire protocol: `hangar-proto::TransponderControl` (WatchTools,
//! CallTool — identical shapes to airlock/mainframe). Auth: SA token,
//! audience `hangar.transponder.sycophant.md`, verified via
//! TokenReview.

use std::sync::Arc;

use hangar_proto::convert::provider_message_to_proto;
use hangar_proto::transponder_control_server::TransponderControl;
use hangar_proto::{
    CallToolRequest, CallToolResponse, CancelTurnRequest, CancelTurnResponse, ConversationSummary,
    DeleteConversationRequest, DeleteConversationResponse, GetConversationHistoryRequest,
    GetConversationHistoryResponse, HistoryEntry, ListConversationsRequest,
    ListConversationsResponse, MintConversationRequest, MintConversationResponse,
    SetConversationNameRequest, SetConversationNameResponse, ToolInfo, ToolListUpdate,
    WatchToolsRequest,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::clients::HangarClient;
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

/// Service impl. Cloning is cheap (Arc-shared router + registry,
/// cheap-clone hangar).
#[derive(Clone)]
pub(crate) struct TransponderService {
    router: Arc<ToolRouter>,
    hangar: HangarClient,
    registry: Arc<ConversationRegistry>,
}

impl TransponderService {
    pub(crate) fn new(
        router: Arc<ToolRouter>,
        hangar: HangarClient,
        registry: Arc<ConversationRegistry>,
    ) -> Self {
        Self {
            router,
            hangar,
            registry,
        }
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
        let mut hangar = self.hangar.clone();
        // Client-driven CallTool has no reply_channel context — these
        // calls do not originate from a chat turn. Channel-source tools
        // would fail with "no reply_channel" which is the correct
        // behaviour: the user can't trigger RevealPath at themselves.
        match self
            .router
            .call_tool(
                &req.name,
                &req.input_json,
                &mut hangar,
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

    async fn mint_conversation(
        &self,
        _request: Request<MintConversationRequest>,
    ) -> Result<Response<MintConversationResponse>, Status> {
        let conversation_id = self
            .registry
            .mint()
            .await
            .map_err(|e| Status::internal(format!("failed to mint conversation: {e}")))?;
        Ok(Response::new(MintConversationResponse { conversation_id }))
    }

    async fn list_conversations(
        &self,
        _request: Request<ListConversationsRequest>,
    ) -> Result<Response<ListConversationsResponse>, Status> {
        // Single-workspace: the request body's `workspace` is informational
        // and ignored — every conversation in the registry belongs to this
        // transponder's workspace.
        let conversations = self
            .registry
            .list_summaries()
            .await
            .into_iter()
            .map(|(id, ts, name)| ConversationSummary {
                conversation_id: id,
                last_touched_ms_epoch: ts,
                name,
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
        // into running chambers/subagents.
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
        if !self.registry.owns(&req.conversation_id).await {
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
                message: Some(provider_message_to_proto(&e.message)),
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
    use crate::clients::HangarClient;
    use crate::conversation::{ConversationStoreFactory, LocalFsFactory};

    /// A service whose registry owns nothing: fresh tempdir-backed factory,
    /// no conversations minted. The hangar handle is a never-dialing lazy
    /// channel — `cancel_turn`'s ownership-reject path never touches it.
    fn service_owning_nothing() -> TransponderService {
        let root = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(root));
        let registry = Arc::new(ConversationRegistry::new(factory));
        let router = Arc::new(ToolRouter::new(None, None, None, registry.clone()));
        TransponderService::new(router, HangarClient::test_lazy(), registry)
    }

    // EARS: "Where the CancelTurn's conversation_id is not owned by this
    // transponder, the transponder shall reject the request with NotFound."
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
