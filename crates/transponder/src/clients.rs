use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::{
    CallToolRequest, CallToolResponse, ToolInfo, ToolListUpdate, WatchToolsRequest,
};
use mainframe_proto::mainframe_runtime_client::MainframeRuntimeClient;
use tightbeam_proto::tightbeam_controller_client::TightbeamControllerClient;
use tightbeam_proto::{SubscribeRequest, TurnEvent, TurnRequest, UserMessage};
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use tonic::{Status, Streaming};

const SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

#[derive(Clone)]
struct SaTokenInterceptor;

impl tonic::service::Interceptor for SaTokenInterceptor {
    fn call(&mut self, mut request: tonic::Request<()>) -> Result<tonic::Request<()>, Status> {
        if let Ok(token) = std::fs::read_to_string(SA_TOKEN_PATH) {
            if let Ok(val) = format!("Bearer {}", token.trim()).parse() {
                request.metadata_mut().insert("authorization", val);
            }
        }
        Ok(request)
    }
}

type AuthenticatedChannel = InterceptedService<Channel, SaTokenInterceptor>;

/// Connect to a tonic gRPC service with backoff retry. The label flows into
/// the retry-event log line and the connect-error message so the caller can
/// see which service failed.
async fn connect_with_retry(addr: &str, label: &'static str) -> Result<Channel, String> {
    let addr = addr.to_string();
    shared::retry_with_backoff(10, label, |_| {
        let addr = addr.clone();
        async move {
            Channel::from_shared(addr.clone())
                .map_err(|e| format!("invalid endpoint: {e}"))?
                .connect()
                .await
                .map_err(|e| format!("failed to connect to {label} at {addr}: {e}"))
        }
    })
    .await
}

pub(crate) struct TightbeamClient {
    inner: TightbeamControllerClient<AuthenticatedChannel>,
}

impl TightbeamClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let channel = connect_with_retry(addr, "tightbeam").await?;
        let inner = TightbeamControllerClient::with_interceptor(channel, SaTokenInterceptor);
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
}

pub(crate) struct ToolClient {
    inner: MainframeRuntimeClient<Channel>,
}

impl ToolClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let channel = connect_with_retry(addr, "mainframe-runtime").await?;
        Ok(Self {
            inner: MainframeRuntimeClient::new(channel),
        })
    }

    pub(crate) async fn list_tools(&mut self) -> Result<Vec<ToolInfo>, String> {
        self.inner
            .list_tools(mainframe_proto::ListToolsRequest {})
            .await
            .map(|resp| {
                resp.into_inner()
                    .tools
                    .into_iter()
                    .map(|t| ToolInfo {
                        name: t.name,
                        description: t.description,
                        parameters_json: t.parameters_json,
                    })
                    .collect()
            })
            .map_err(|e| format!("list_tools RPC failed: {e}"))
    }

    pub(crate) async fn call_tool(
        &mut self,
        name: &str,
        input_json: &str,
    ) -> Result<CallToolResponse, String> {
        self.inner
            .call_tool(mainframe_proto::CallToolRequest {
                name: name.to_string(),
                input_json: input_json.to_string(),
            })
            .await
            .map(|resp| {
                let inner = resp.into_inner();
                CallToolResponse {
                    output: inner.output,
                    is_error: inner.is_error,
                }
            })
            .map_err(|e| format!("call_tool RPC failed: {e}"))
    }

    /// Construct a `ToolClient` whose inner channel never connects. For unit
    /// tests that need a `ToolRouter` but exercise only in-memory paths
    /// (`apply_airlock_tools`, `tool_definitions`) — neither of which touches
    /// the mainframe channel.
    #[cfg(test)]
    pub(crate) fn stub_for_tests() -> Self {
        let channel = Channel::from_static("http://localhost:1").connect_lazy();
        Self {
            inner: MainframeRuntimeClient::new(channel),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AirlockClient {
    inner: AirlockControllerClient<AuthenticatedChannel>,
}

impl AirlockClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let channel = connect_with_retry(addr, "airlock").await?;
        let inner = AirlockControllerClient::with_interceptor(channel, SaTokenInterceptor);
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

#[cfg(test)]
mod tool_client_tests {
    use super::*;
    use mainframe_proto::mainframe_runtime_server::{MainframeRuntime, MainframeRuntimeServer};
    use tokio_stream::wrappers::TcpListenerStream;

    struct StubMainframeRuntime;

    #[tonic::async_trait]
    impl MainframeRuntime for StubMainframeRuntime {
        async fn list_tools(
            &self,
            _: tonic::Request<mainframe_proto::ListToolsRequest>,
        ) -> Result<tonic::Response<mainframe_proto::ListToolsResponse>, tonic::Status> {
            Ok(tonic::Response::new(mainframe_proto::ListToolsResponse {
                tools: vec![mainframe_proto::ToolInfo {
                    name: "stub".into(),
                    ..Default::default()
                }],
            }))
        }

        async fn call_tool(
            &self,
            _: tonic::Request<mainframe_proto::CallToolRequest>,
        ) -> Result<tonic::Response<mainframe_proto::CallToolResponse>, tonic::Status> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn tool_client_connects_over_tcp_loopback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(MainframeRuntimeServer::new(StubMainframeRuntime))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
        });

        let mut client = ToolClient::connect(&format!("http://{addr}"))
            .await
            .unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "stub");
    }
}
