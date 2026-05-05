use std::path::Path;

use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::{CallToolRequest, CallToolResponse, ListToolsRequest, ToolInfo};
use tightbeam_proto::tightbeam_controller_client::TightbeamControllerClient;
use tightbeam_proto::{SubscribeRequest, TurnEvent, TurnRequest, UserMessage};
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint, Uri};
use tonic::{Status, Streaming};
use tower::service_fn;

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
    pub(crate) async fn connect_uds(socket_path: &Path) -> Result<Self, String> {
        let socket_path = socket_path.to_path_buf();
        let channel = shared::retry_with_backoff(10, "mainframe-runtime-connect", |_| {
            let path = socket_path.clone();
            async move {
                Endpoint::try_from("http://[::]:50051")
                    .map_err(|e| format!("invalid endpoint: {e}"))?
                    .connect_with_connector(service_fn(move |_: Uri| {
                        let path = path.clone();
                        async move {
                            let stream = tokio::net::UnixStream::connect(path).await?;
                            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                        }
                    }))
                    .await
                    .map_err(|e| format!("failed to connect to mainframe-runtime socket: {e}"))
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
