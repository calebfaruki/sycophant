use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::{AwaitToolResultRequest, CancelToolCallRequest};
use hangar_proto::hangar_controller_client::HangarControllerClient;
use hangar_proto::{ContentBlock, TurnEvent, TurnRequest, TurnStateEvent};
use mainframe_proto::mainframe_controller_client::MainframeControllerClient;
use mainframe_proto::{AgentInfo, GetAgentRequest, ListAgentsRequest};
use proto_common::{
    CallToolRequest, CallToolResponse, CancelTurnRequest, SendServerNotificationRequest,
    SendServerRequestAndAwaitRequest, StreamItem, SubscribeRequest, ToolListUpdate,
    ToolResultFrame, UserMessage, WatchToolsRequest,
};
use relay_proto::relay_internal_client::RelayInternalClient;
use relay_proto::{ChannelReply, DeliverOutboundRequest, DeliverStreamItemRequest};
use shared::auth::{
    SaTokenInterceptor, TRANSPONDER_AIRLOCK_TOKEN_PATH, TRANSPONDER_HANGAR_TOKEN_PATH,
    TRANSPONDER_MAINFRAME_TOKEN_PATH, TRANSPONDER_RELAY_TOKEN_PATH,
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

/// One tool call's stream of typed output frames. Real impl wraps
/// `tonic::Streaming`; tests back it with a `VecDeque`. Mirrors `TurnSource`.
#[async_trait::async_trait]
pub(crate) trait ToolResultStream: Send {
    async fn next_frame(&mut self) -> Option<Result<ToolResultFrame, String>>;
}

pub(crate) struct TonicToolResultStream(Streaming<ToolResultFrame>);

#[async_trait::async_trait]
impl ToolResultStream for TonicToolResultStream {
    async fn next_frame(&mut self) -> Option<Result<ToolResultFrame, String>> {
        self.0
            .next()
            .await
            .map(|r| r.map_err(|e| format!("frame stream error: {e}")))
    }
}

/// Outcome of a `send_server_request_and_await`. Mirrors the controller
/// response variants but uses Rust-native types so the transponder can
/// pattern-match without parsing protobuf optionals everywhere.
#[derive(Debug)]
pub(crate) enum ServerRequestOutcome {
    Result(String),
    Error { code: i32, message: String },
    TimedOut,
    UnknownChannel,
    UnsupportedMethod,
}

/// RPC surface the LLM loop needs from hangar-controller: stateless LLM
/// dispatch. Conversation minting moved to the transponder's local
/// registry — hangar no longer owns a conversation store.
#[async_trait::async_trait]
pub(crate) trait HangarRpc: Send {
    async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String>;
    /// Best-effort cancel of the in-flight turn keyed by `conversation_id`. The
    /// controller scopes the key by the caller's workspace; an unknown or
    /// already-finished id is a safe no-op.
    async fn cancel_turn(&mut self, conversation_id: &str) -> Result<(), String>;
}

/// RPC surface the LLM loop needs from the relay gateway: pushing
/// server→client requests over a registered channel.
#[async_trait::async_trait]
pub(crate) trait RelayRpc: Send {
    /// Push a fire-and-forget `ServerRequest` to the named channel. The
    /// returned bool is best-effort — true means the gateway successfully
    /// enqueued the frame; false means it rejected (unknown channel,
    /// unsupported method).
    async fn send_server_notification(
        &mut self,
        channel_id: &str,
        method: &str,
        params_json: &str,
    ) -> Result<bool, String>;
    /// Push a `ServerRequest` and block on the matching `ClientResponse`.
    /// `timeout_seconds = 0` lets the gateway pick a default.
    async fn send_server_request_and_await(
        &mut self,
        channel_id: &str,
        request_id: &str,
        method: &str,
        params_json: &str,
        timeout_seconds: u32,
    ) -> Result<ServerRequestOutcome, String>;
    /// Push one streamed activity frame produced during a turn. The gateway
    /// relays the `StreamItem` to the client unchanged. Returns the gateway's
    /// best-effort `delivered` bool.
    async fn deliver_stream_item(
        &mut self,
        channel_id: &str,
        item: StreamItem,
    ) -> Result<bool, String>;
}

#[derive(Clone)]
pub(crate) struct HangarClient {
    inner: HangarControllerClient<AuthenticatedChannel>,
}

impl HangarClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let channel = shared::grpc_client::connect_with_keepalive(addr, "hangar").await?;
        let inner = HangarControllerClient::with_interceptor(
            channel,
            SaTokenInterceptor::new(TRANSPONDER_HANGAR_TOKEN_PATH),
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
}

#[async_trait::async_trait]
impl HangarRpc for HangarClient {
    async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
        let stream = HangarClient::turn(self, request).await?;
        Ok(Box::new(TonicTurnSource(stream)))
    }

    async fn cancel_turn(&mut self, conversation_id: &str) -> Result<(), String> {
        // Fire-and-forget on a cloned handle: spawn the best-effort cancel RPC
        // so the turn's terminal path never blocks on its completion. Mirrors
        // the tool router's AirlockClient clone-into-spawn shape.
        let mut inner = self.inner.clone();
        let conversation_id = conversation_id.to_string();
        tokio::spawn(async move {
            let _ = inner
                .cancel_turn(CancelTurnRequest { conversation_id })
                .await;
        });
        Ok(())
    }
}

/// Client for the relay gateway's internal listener. Carries the
/// `transponder.relay` SA token; multiplexes Subscribe (inbound user
/// messages) and the channel server-request methods over one HTTP/2
/// connection.
#[derive(Clone)]
pub(crate) struct RelayClient {
    inner: RelayInternalClient<AuthenticatedChannel>,
}

impl RelayClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let channel = shared::grpc_client::connect_with_keepalive(addr, "relay").await?;
        let inner = RelayInternalClient::with_interceptor(
            channel,
            SaTokenInterceptor::new(TRANSPONDER_RELAY_TOKEN_PATH),
        );
        Ok(Self { inner })
    }

    pub(crate) async fn subscribe(&mut self) -> Result<Streaming<UserMessage>, String> {
        self.inner
            .subscribe(SubscribeRequest {})
            .await
            .map(|resp| resp.into_inner())
            .map_err(|e| format!("subscribe RPC failed: {e}"))
    }

    /// Push the assistant reply and/or terminal turn-state to the client via
    /// the gateway. The transponder is the sole originator of replies (the
    /// gateway enqueues the reply before applying the turn-state, preserving
    /// client-visible ordering).
    pub(crate) async fn deliver_outbound(
        &mut self,
        channel_id: &str,
        conversation_id: &str,
        reply: Option<Vec<ContentBlock>>,
        turn_state: Option<TurnStateEvent>,
    ) -> Result<bool, String> {
        let resp = self
            .inner
            .deliver_outbound(DeliverOutboundRequest {
                channel_id: channel_id.to_string(),
                conversation_id: conversation_id.to_string(),
                reply: reply.map(|content| ChannelReply { content }),
                turn_state,
            })
            .await
            .map_err(|e| format!("deliver_outbound RPC failed: {e}"))?
            .into_inner();
        Ok(resp.delivered)
    }
}

#[async_trait::async_trait]
impl RelayRpc for RelayClient {
    async fn send_server_notification(
        &mut self,
        channel_id: &str,
        method: &str,
        params_json: &str,
    ) -> Result<bool, String> {
        let resp = self
            .inner
            .send_server_notification(SendServerNotificationRequest {
                channel_id: channel_id.to_string(),
                method: method.to_string(),
                params_json: params_json.to_string(),
            })
            .await
            .map_err(|e| format!("send_server_notification RPC failed: {e}"))?
            .into_inner();
        Ok(resp.delivered)
    }

    async fn send_server_request_and_await(
        &mut self,
        channel_id: &str,
        request_id: &str,
        method: &str,
        params_json: &str,
        timeout_seconds: u32,
    ) -> Result<ServerRequestOutcome, String> {
        let resp = self
            .inner
            .send_server_request_and_await(SendServerRequestAndAwaitRequest {
                channel_id: channel_id.to_string(),
                request_id: request_id.to_string(),
                method: method.to_string(),
                params_json: params_json.to_string(),
                timeout_seconds,
            })
            .await
            .map_err(|e| format!("send_server_request_and_await RPC failed: {e}"))?
            .into_inner();
        if resp.timed_out {
            Ok(ServerRequestOutcome::TimedOut)
        } else if resp.unknown_channel {
            Ok(ServerRequestOutcome::UnknownChannel)
        } else if resp.unsupported_method {
            Ok(ServerRequestOutcome::UnsupportedMethod)
        } else if let Some(err) = resp.error {
            Ok(ServerRequestOutcome::Error {
                code: err.code,
                message: err.message,
            })
        } else {
            Ok(ServerRequestOutcome::Result(resp.result_json))
        }
    }

    async fn deliver_stream_item(
        &mut self,
        channel_id: &str,
        item: StreamItem,
    ) -> Result<bool, String> {
        let resp = self
            .inner
            .deliver_stream_item(DeliverStreamItemRequest {
                channel_id: channel_id.to_string(),
                item: Some(item),
            })
            .await
            .map_err(|e| format!("deliver_stream_item RPC failed: {e}"))?
            .into_inner();
        Ok(resp.delivered)
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

    pub(crate) async fn begin_tool_call(
        &mut self,
        name: &str,
        input_json: &str,
    ) -> Result<String, String> {
        self.inner
            .begin_tool_call(CallToolRequest {
                name: name.to_string(),
                input_json: input_json.to_string(),
                // The chamber just executes the tool; the transponder owns the
                // conversation-scoped execution log, so this outbound call
                // carries no conversation_id.
                conversation_id: String::new(),
            })
            .await
            .map(|resp| resp.into_inner().call_id)
            .map_err(|e| format!("begin_tool_call RPC failed: {e}"))
    }

    pub(crate) async fn await_tool_result(
        &mut self,
        call_id: &str,
    ) -> Result<Streaming<ToolResultFrame>, String> {
        self.inner
            .await_tool_result(AwaitToolResultRequest {
                call_id: call_id.to_string(),
            })
            .await
            .map(|resp| resp.into_inner())
            .map_err(|e| format!("await_tool_result RPC failed: {e}"))
    }

    pub(crate) async fn cancel_tool_call(&mut self, call_id: &str) -> Result<bool, String> {
        self.inner
            .cancel_tool_call(CancelToolCallRequest {
                call_id: call_id.to_string(),
            })
            .await
            .map(|resp| resp.into_inner().cancelled)
            .map_err(|e| format!("cancel_tool_call RPC failed: {e}"))
    }
}

/// Subset of `AirlockClient` the tool router's `Source::Airlock` arm depends
/// on: the begin/await/cancel split that lets the caller learn a call_id, open
/// the result frame stream, race it against the turn's cancel token, and fire a
/// cancel. A trait so tests back the arm with a `FakeAirlock` recorder without a
/// live gRPC server.
#[async_trait::async_trait]
pub(crate) trait AirlockRpc: Send {
    async fn begin_tool_call(&mut self, name: &str, input_json: &str) -> Result<String, String>;
    async fn await_tool_result(
        &mut self,
        call_id: &str,
    ) -> Result<Box<dyn ToolResultStream>, String>;
    async fn cancel_tool_call(&mut self, call_id: &str) -> Result<bool, String>;
}

#[async_trait::async_trait]
impl AirlockRpc for AirlockClient {
    async fn begin_tool_call(&mut self, name: &str, input_json: &str) -> Result<String, String> {
        AirlockClient::begin_tool_call(self, name, input_json).await
    }
    async fn await_tool_result(
        &mut self,
        call_id: &str,
    ) -> Result<Box<dyn ToolResultStream>, String> {
        let stream = AirlockClient::await_tool_result(self, call_id).await?;
        Ok(Box::new(TonicToolResultStream(stream)))
    }
    async fn cancel_tool_call(&mut self, call_id: &str) -> Result<bool, String> {
        AirlockClient::cancel_tool_call(self, call_id).await
    }
}

/// Client for mainframe-controller. Mirrors `AirlockClient`'s shape: one
/// HTTP/2 channel multiplexed across `watch_tools`/`call_tool` and the two
/// internal RPCs the transponder uses for persona content.
#[derive(Clone)]
pub(crate) struct MainframeClient {
    inner: MainframeControllerClient<AuthenticatedChannel>,
}

impl MainframeClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let channel = shared::grpc_client::connect_with_keepalive(addr, "mainframe").await?;
        let inner = MainframeControllerClient::with_interceptor(
            channel,
            SaTokenInterceptor::new(TRANSPONDER_MAINFRAME_TOKEN_PATH),
        );
        Ok(Self { inner })
    }

    pub(crate) async fn watch_tools(&mut self) -> Result<Streaming<ToolListUpdate>, String> {
        self.inner
            .watch_tools(WatchToolsRequest {})
            .await
            .map(|resp| resp.into_inner())
            .map_err(|e| format!("mainframe watch_tools RPC failed: {e}"))
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
                // The mainframe just executes the tool; the conversation-scoped
                // execution log is the transponder's, so no conversation_id.
                conversation_id: String::new(),
            })
            .await
            .map(|resp| resp.into_inner())
            .map_err(|e| format!("mainframe call_tool RPC failed: {e}"))
    }

    /// Fetch persona content. Empty `name` -> primary `AGENTS.md`;
    /// non-empty -> `agents/<name>.md`.
    pub(crate) async fn get_agent(&mut self, name: &str) -> Result<String, String> {
        self.inner
            .get_agent(GetAgentRequest {
                name: name.to_string(),
            })
            .await
            .map(|resp| resp.into_inner().content)
            .map_err(|e| format!("get_agent RPC failed: {e}"))
    }

    /// Enumerate available sub-agents with their descriptions.
    pub(crate) async fn list_agents(&mut self) -> Result<Vec<AgentInfo>, String> {
        self.inner
            .list_agents(ListAgentsRequest {})
            .await
            .map(|resp| resp.into_inner().agents)
            .map_err(|e| format!("list_agents RPC failed: {e}"))
    }
}

/// Subset of `MainframeClient` the runtime tools depend on. Allows tests
/// to back the calls with a fake recorder without spinning up a real
/// gRPC server.
#[async_trait::async_trait]
pub(crate) trait MainframeRpc: Send {
    async fn get_agent(&mut self, name: &str) -> Result<String, String>;
    async fn list_agents(&mut self) -> Result<Vec<AgentInfo>, String>;
    async fn call_tool(&mut self, name: &str, input_json: &str)
        -> Result<CallToolResponse, String>;
}

#[async_trait::async_trait]
impl MainframeRpc for MainframeClient {
    async fn get_agent(&mut self, name: &str) -> Result<String, String> {
        MainframeClient::get_agent(self, name).await
    }
    async fn list_agents(&mut self) -> Result<Vec<AgentInfo>, String> {
        MainframeClient::list_agents(self).await
    }
    async fn call_tool(
        &mut self,
        name: &str,
        input_json: &str,
    ) -> Result<CallToolResponse, String> {
        MainframeClient::call_tool(self, name, input_json).await
    }
}
