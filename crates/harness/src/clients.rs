use proto_common::{
    ContentBlock, SendServerNotificationRequest, SendServerRequestAndAwaitRequest, StreamItem,
    SubscribeRequest, TurnStateEvent, UserMessage,
};
use relay_proto::relay_internal_client::RelayInternalClient;
use relay_proto::{DeliverReplyRequest, DeliverStreamItemRequest, DeliverTurnStateRequest};
use shared::auth::{SaTokenInterceptor, HARNESS_RELAY_TOKEN_PATH};
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use tonic::Streaming;
use toolset_proto::{TurnEvent, TurnRequest};

type AuthenticatedChannel = InterceptedService<Channel, SaTokenInterceptor>;

/// One LLM turn's stream of events. The live model call is served by
/// `InferenceDispatch`, which dials the model directly; tests back this with a
/// `VecDeque`.
#[async_trait::async_trait]
pub(crate) trait TurnSource: Send {
    async fn next_event(&mut self) -> Option<Result<TurnEvent, String>>;
}

/// Outcome of a `send_server_request_and_await`. Mirrors the controller
/// response variants but uses Rust-native types so the harness can
/// pattern-match without parsing protobuf optionals everywhere.
#[derive(Debug)]
pub(crate) enum ServerRequestOutcome {
    Result(String),
    Error { code: i32, message: String },
    TimedOut,
    UnknownChannelStream,
    UnsupportedMethod,
}

/// The harness's model-call seam. `turn`/`cancel_turn` are served by
/// `InferenceDispatch`, which dials the model directly. Tests back the seam with
/// a fake without a live gRPC server. Conversation minting lives in the
/// harness's local registry.
#[async_trait::async_trait]
pub(crate) trait ToolsetRpc: Send {
    async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String>;
    /// Best-effort cancel of the in-flight turn keyed by `conversation_id`. An
    /// unknown or already-finished id is a safe no-op.
    async fn cancel_turn(&mut self, conversation_id: &str) -> Result<(), String>;
}

/// RPC surface the LLM loop needs from the relay gateway: pushing
/// server→client requests over a registered channel stream.
#[async_trait::async_trait]
pub(crate) trait RelayRpc: Send {
    /// Push a fire-and-forget `ServerRequest` to all channel streams for the agent.
    async fn send_server_notification(
        &mut self,
        agent: &str,
        method: &str,
        params_json: &str,
    ) -> Result<bool, String>;
    /// Push a `ServerRequest` and block on the matching `ClientResponse`.
    /// `timeout_seconds = 0` lets the gateway pick a default.
    async fn send_server_request_and_await(
        &mut self,
        agent: &str,
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
        channel_stream_id: &str,
        item: StreamItem,
    ) -> Result<bool, String>;
    /// Push the assistant reply (ChannelStreamSend) to all agent channel streams.
    async fn deliver_reply(
        &mut self,
        conversation_id: &str,
        content: Vec<ContentBlock>,
    ) -> Result<bool, String>;
    /// Push a turn-state event. The relay records the state keyed by
    /// the subject carried in the request.
    async fn deliver_turn_state(
        &mut self,
        turn_state: TurnStateEvent,
        subject: &str,
    ) -> Result<bool, String>;
}

/// Client for the relay gateway's internal listener. Carries the
/// `harness.relay` SA token; multiplexes Subscribe (inbound user
/// messages) and the channel-stream server-request methods over one HTTP/2
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
            SaTokenInterceptor::new(HARNESS_RELAY_TOKEN_PATH),
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
}

#[async_trait::async_trait]
impl RelayRpc for RelayClient {
    async fn send_server_notification(
        &mut self,
        agent: &str,
        method: &str,
        params_json: &str,
    ) -> Result<bool, String> {
        let resp = self
            .inner
            .send_server_notification(SendServerNotificationRequest {
                method: method.to_string(),
                params_json: params_json.to_string(),
                agent: agent.to_string(),
            })
            .await
            .map_err(|e| format!("send_server_notification RPC failed: {e}"))?
            .into_inner();
        Ok(resp.delivered)
    }

    async fn send_server_request_and_await(
        &mut self,
        agent: &str,
        request_id: &str,
        method: &str,
        params_json: &str,
        timeout_seconds: u32,
    ) -> Result<ServerRequestOutcome, String> {
        let resp = self
            .inner
            .send_server_request_and_await(SendServerRequestAndAwaitRequest {
                request_id: request_id.to_string(),
                method: method.to_string(),
                params_json: params_json.to_string(),
                timeout_seconds,
                agent: agent.to_string(),
            })
            .await
            .map_err(|e| format!("send_server_request_and_await RPC failed: {e}"))?
            .into_inner();
        if resp.timed_out {
            Ok(ServerRequestOutcome::TimedOut)
        } else if resp.unknown_channel_stream {
            Ok(ServerRequestOutcome::UnknownChannelStream)
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
        channel_stream_id: &str,
        item: StreamItem,
    ) -> Result<bool, String> {
        let resp = self
            .inner
            .deliver_stream_item(DeliverStreamItemRequest {
                channel_stream_id: channel_stream_id.to_string(),
                item: Some(item),
            })
            .await
            .map_err(|e| format!("deliver_stream_item RPC failed: {e}"))?
            .into_inner();
        Ok(resp.delivered)
    }

    async fn deliver_reply(
        &mut self,
        conversation_id: &str,
        content: Vec<ContentBlock>,
    ) -> Result<bool, String> {
        let resp = self
            .inner
            .deliver_reply(DeliverReplyRequest {
                conversation_id: conversation_id.to_string(),
                content,
            })
            .await
            .map_err(|e| format!("deliver_reply RPC failed: {e}"))?
            .into_inner();
        Ok(resp.delivered)
    }

    async fn deliver_turn_state(
        &mut self,
        turn_state: TurnStateEvent,
        subject: &str,
    ) -> Result<bool, String> {
        let resp = self
            .inner
            .deliver_turn_state(DeliverTurnStateRequest {
                channel_stream_id: String::new(),
                turn_state: Some(turn_state),
                subject: subject.to_string(),
            })
            .await
            .map_err(|e| format!("deliver_turn_state RPC failed: {e}"))?
            .into_inner();
        Ok(resp.delivered)
    }
}
