use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::{CallToolRequest, CallToolResponse, ListToolsRequest, ToolInfo};
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

pub(crate) struct TightbeamClient {
    inner: TightbeamControllerClient<AuthenticatedChannel>,
}

impl TightbeamClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let addr = addr.to_string();
        let channel = shared::retry_with_backoff(10, "tightbeam-connect", |_| {
            let addr = addr.clone();
            async move {
                Channel::from_shared(addr.clone())
                    .map_err(|e| format!("invalid endpoint: {e}"))?
                    .connect()
                    .await
                    .map_err(|e| format!("failed to connect to tightbeam at {addr}: {e}"))
            }
        })
        .await?;
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
    inner: AirlockControllerClient<Channel>,
}

impl ToolClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let addr = addr.to_string();
        let channel = shared::retry_with_backoff(10, "mainframe-runtime-connect", |_| {
            let addr = addr.clone();
            async move {
                Channel::from_shared(addr.clone())
                    .map_err(|e| format!("invalid endpoint: {e}"))?
                    .connect()
                    .await
                    .map_err(|e| format!("failed to connect to mainframe-runtime at {addr}: {e}"))
            }
        })
        .await?;
        Ok(Self {
            inner: AirlockControllerClient::new(channel),
        })
    }

    pub(crate) async fn list_tools(&mut self) -> Result<Vec<ToolInfo>, String> {
        self.inner
            .list_tools(ListToolsRequest {})
            .await
            .map(|resp| resp.into_inner().tools)
            .map_err(|e| format!("list_tools RPC failed: {e}"))
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

pub(crate) struct AirlockClient {
    inner: AirlockControllerClient<AuthenticatedChannel>,
}

impl AirlockClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        let addr = addr.to_string();
        let channel = shared::retry_with_backoff(10, "airlock-connect", |_| {
            let addr = addr.clone();
            async move {
                Channel::from_shared(addr.clone())
                    .map_err(|e| format!("invalid endpoint: {e}"))?
                    .connect()
                    .await
                    .map_err(|e| format!("failed to connect to airlock at {addr}: {e}"))
            }
        })
        .await?;
        let inner = AirlockControllerClient::with_interceptor(channel, SaTokenInterceptor);
        Ok(Self { inner })
    }

    pub(crate) async fn list_tools(&mut self) -> Result<Vec<ToolInfo>, String> {
        self.inner
            .list_tools(ListToolsRequest {})
            .await
            .map(|resp| resp.into_inner().tools)
            .map_err(|e| format!("list_tools RPC failed: {e}"))
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
    use airlock_proto::airlock_controller_server::{AirlockController, AirlockControllerServer};
    use airlock_proto::{
        GetToolCallRequest, ListToolsResponse, SendToolResultAck, SendToolResultRequest,
        ToolCallAssignment,
    };
    use tokio_stream::wrappers::TcpListenerStream;

    struct StubAirlockController;

    #[tonic::async_trait]
    impl AirlockController for StubAirlockController {
        async fn list_tools(
            &self,
            _: tonic::Request<ListToolsRequest>,
        ) -> Result<tonic::Response<ListToolsResponse>, tonic::Status> {
            Ok(tonic::Response::new(ListToolsResponse {
                tools: vec![ToolInfo {
                    name: "stub".into(),
                    ..Default::default()
                }],
            }))
        }
        async fn call_tool(
            &self,
            _: tonic::Request<CallToolRequest>,
        ) -> Result<tonic::Response<CallToolResponse>, tonic::Status> {
            unimplemented!()
        }
        async fn get_tool_call(
            &self,
            _: tonic::Request<GetToolCallRequest>,
        ) -> Result<tonic::Response<ToolCallAssignment>, tonic::Status> {
            unimplemented!()
        }
        async fn send_tool_result(
            &self,
            _: tonic::Request<SendToolResultRequest>,
        ) -> Result<tonic::Response<SendToolResultAck>, tonic::Status> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn tool_client_connects_over_tcp_loopback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(AirlockControllerServer::new(StubAirlockController))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
        });

        let mut client = ToolClient::connect(&format!("http://{addr}")).await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "stub");
    }
}
