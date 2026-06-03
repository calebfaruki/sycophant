use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::{CallToolRequest, CallToolResponse, ToolListUpdate, WatchToolsRequest};
use shared::auth::{
    SaTokenInterceptor, TRANSPONDER_AIRLOCK_TOKEN_PATH, TRANSPONDER_TIGHTBEAM_TOKEN_PATH,
};
use tightbeam_proto::tightbeam_controller_client::TightbeamControllerClient;
use tightbeam_proto::{
    GetConversationHistoryRequest, GetConversationHistoryResponse, MintConversationRequest,
    SubscribeRequest, TurnEvent, TurnRequest, UserMessage,
};
use tokio_stream::StreamExt;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use tonic::Streaming;

type AuthenticatedChannel = InterceptedService<Channel, SaTokenInterceptor>;

/// One LLM turn's stream of events. Real impl wraps `tonic::Streaming`;
/// tests back it with a `VecDeque`.
#[async_trait::async_trait]
pub(crate) trait TurnSource: Send {
    async fn next_event(&mut self) -> Option<Result<TurnEvent, String>>;
}

pub(crate) struct TonicTurnSource(Streaming<TurnEvent>);

#[async_trait::async_trait]
impl TurnSource for TonicTurnSource {
    async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
        self.0
            .next()
            .await
            .map(|r| r.map_err(|e| format!("stream error: {e}")))
    }
}

/// RPC surface the LLM loop needs from tightbeam-controller.
#[async_trait::async_trait]
pub(crate) trait TightbeamRpc: Send {
    async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String>;
    async fn mint_conversation(&mut self) -> Result<String, String>;
    async fn get_conversation_history(
        &mut self,
        conversation_id: &str,
        limit: Option<u32>,
    ) -> Result<GetConversationHistoryResponse, String>;
}

pub(crate) struct TightbeamClient {
    inner: TightbeamControllerClient<AuthenticatedChannel>,
}

impl TightbeamClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let channel = shared::grpc_client::connect_with_keepalive(addr, "tightbeam").await?;
        let inner = TightbeamControllerClient::with_interceptor(
            channel,
            SaTokenInterceptor::new(TRANSPONDER_TIGHTBEAM_TOKEN_PATH),
        );
        Ok(Self { inner })
    }

    pub(crate) async fn turn(
        &mut self,
        request: TurnRequest,
    ) -> Result<Streaming<TurnEvent>, String> {
        self.inner
            .turn(request)
            .await
            .map(|resp| resp.into_inner())
            .map_err(|e| format!("turn RPC failed: {e}"))
    }

    pub(crate) async fn subscribe(&mut self) -> Result<Streaming<UserMessage>, String> {
        self.inner
            .subscribe(SubscribeRequest {})
            .await
            .map(|resp| resp.into_inner())
            .map_err(|e| format!("subscribe RPC failed: {e}"))
    }

    /// Ask the controller for a fresh conversation id. Called once per
    /// new chat thread (e.g., transponder process start, delegate
    /// sub-conversation start). The returned id is threaded into every
    /// follow-up TurnRequest belonging to that thread.
    pub(crate) async fn mint_conversation(&mut self) -> Result<String, String> {
        self.inner
            .mint_conversation(MintConversationRequest {})
            .await
            .map(|resp| resp.into_inner().conversation_id)
            .map_err(|e| format!("mint_conversation RPC failed: {e}"))
    }

    /// Fetch the tail of a conversation's history. Backs the
    /// `recent_turns` built-in tool. `limit` of None / Some(0) lets the
    /// controller pick the default; positive values are server-clamped.
    pub(crate) async fn get_conversation_history(
        &mut self,
        conversation_id: &str,
        limit: Option<u32>,
    ) -> Result<GetConversationHistoryResponse, String> {
        self.inner
            .get_conversation_history(GetConversationHistoryRequest {
                conversation_id: conversation_id.to_string(),
                limit,
            })
            .await
            .map(|resp| resp.into_inner())
            .map_err(|e| format!("get_conversation_history RPC failed: {e}"))
    }
}

#[async_trait::async_trait]
impl TightbeamRpc for TightbeamClient {
    async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
        let stream = TightbeamClient::turn(self, request).await?;
        Ok(Box::new(TonicTurnSource(stream)))
    }

    async fn mint_conversation(&mut self) -> Result<String, String> {
        TightbeamClient::mint_conversation(self).await
    }

    async fn get_conversation_history(
        &mut self,
        conversation_id: &str,
        limit: Option<u32>,
    ) -> Result<GetConversationHistoryResponse, String> {
        TightbeamClient::get_conversation_history(self, conversation_id, limit).await
    }
}

#[derive(Clone)]
pub(crate) struct AirlockClient {
    inner: AirlockControllerClient<AuthenticatedChannel>,
}

impl AirlockClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let channel = shared::grpc_client::connect_with_keepalive(addr, "airlock").await?;
        let inner = AirlockControllerClient::with_interceptor(
            channel,
            SaTokenInterceptor::new(TRANSPONDER_AIRLOCK_TOKEN_PATH),
        );
        Ok(Self { inner })
    }

    pub(crate) async fn watch_tools(&mut self) -> Result<Streaming<ToolListUpdate>, String> {
        self.inner
            .watch_tools(WatchToolsRequest {})
            .await
            .map(|resp| resp.into_inner())
            .map_err(|e| format!("watch_tools RPC failed: {e}"))
    }

    pub(crate) async fn call_tool(
        &mut self,
        name: &str,
        input_json: &str,
    ) -> Result<CallToolResponse, String> {
        self.inner
            .call_tool(CallToolRequest {
                name: name.to_string(),
                input_json: input_json.to_string(),
            })
            .await
            .map(|resp| resp.into_inner())
            .map_err(|e| format!("call_tool RPC failed: {e}"))
    }
}
