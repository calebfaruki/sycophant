use std::path::Path;

use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::{CallToolRequest, CallToolResponse, ListToolsRequest, ToolInfo};
use pkm_proto::pkm_service_client::PkmServiceClient;
use pkm_proto::{
    transponder_event, PkmEvent, ReportSystemTurn, TransponderEvent, UserMessage as PkmUserMessage,
};
use tightbeam_proto::tightbeam_controller_client::TightbeamControllerClient;
use tightbeam_proto::{ContentBlock, SubscribeRequest, TurnEvent, TurnRequest, UserMessage};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
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
        for attempt in 1..=10 {
            let result = Channel::from_shared(addr.to_string())
                .map_err(|e| format!("invalid endpoint: {e}"))?
                .connect()
                .await;
            match result {
                Ok(channel) => {
                    let inner =
                        TightbeamControllerClient::with_interceptor(channel, SaTokenInterceptor);
                    return Ok(Self { inner });
                }
                Err(e) if attempt < 10 => {
                    tracing::warn!(attempt, addr, error = %e, "retrying tightbeam connection");
                    tokio::time::sleep(std::time::Duration::from_secs(attempt)).await;
                }
                Err(e) => return Err(format!("failed to connect to tightbeam at {addr}: {e}")),
            }
        }
        unreachable!()
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
        let socket_display = socket_path.display().to_string();
        for attempt in 1..=10 {
            let path = socket_path.to_path_buf();
            let result = Endpoint::try_from("http://[::]:50051")
                .map_err(|e| format!("invalid endpoint: {e}"))?
                .connect_with_connector(service_fn(move |_: Uri| {
                    let path = path.clone();
                    async move {
                        let stream = tokio::net::UnixStream::connect(path).await?;
                        Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                    }
                }))
                .await;
            match result {
                Ok(channel) => {
                    return Ok(Self {
                        inner: AirlockControllerClient::new(channel),
                    });
                }
                Err(e) if attempt < 10 => {
                    tracing::warn!(attempt, socket = %socket_display, error = %e, "retrying workspace-tools connection");
                    tokio::time::sleep(std::time::Duration::from_secs(attempt)).await;
                }
                Err(e) => {
                    return Err(format!("failed to connect to workspace-tools socket: {e}"));
                }
            }
        }
        unreachable!()
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

pub(crate) struct PkmClient {
    inner: PkmServiceClient<AuthenticatedChannel>,
}

impl PkmClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        for attempt in 1..=10 {
            let result = Channel::from_shared(addr.to_string())
                .map_err(|e| format!("invalid endpoint: {e}"))?
                .connect()
                .await;
            match result {
                Ok(channel) => {
                    let inner = PkmServiceClient::with_interceptor(channel, SaTokenInterceptor);
                    return Ok(Self { inner });
                }
                Err(e) if attempt < 10 => {
                    tracing::warn!(attempt, addr, error = %e, "retrying pkm connection");
                    tokio::time::sleep(std::time::Duration::from_secs(attempt)).await;
                }
                Err(e) => return Err(format!("failed to connect to pkm at {addr}: {e}")),
            }
        }
        unreachable!()
    }

    /// Open a ResolveTurn session with the user message pre-sent.
    ///
    /// The first event MUST be queued before awaiting the RPC: PKM's handler
    /// blocks on the first message before returning Response<Stream>, so the
    /// client must populate the request stream before awaiting the response
    /// or both sides deadlock.
    pub(crate) async fn resolve_turn(
        &mut self,
        content: Vec<ContentBlock>,
        sender: String,
    ) -> Result<ResolveTurnSession, String> {
        // Strict request-response: at most 2 client-side events per session.
        let (tx, rx) = mpsc::channel::<TransponderEvent>(2);
        tx.send(TransponderEvent {
            event: Some(transponder_event::Event::UserMessage(PkmUserMessage {
                content,
                sender,
            })),
        })
        .await
        .map_err(|_| "pkm session closed before send")?;
        let response = self
            .inner
            .resolve_turn(ReceiverStream::new(rx))
            .await
            .map_err(|e| format!("resolve_turn RPC failed: {e}"))?;
        Ok(ResolveTurnSession {
            tx,
            rx: response.into_inner(),
        })
    }
}

/// Wraps the bidi pair for a single ResolveTurn invocation. The session is
/// opened with `PkmClient::resolve_turn(content, sender)`, which pre-sends the
/// `UserMessage` before awaiting the RPC.
pub(crate) struct ResolveTurnSession {
    tx: mpsc::Sender<TransponderEvent>,
    rx: Streaming<PkmEvent>,
}

impl ResolveTurnSession {
    pub(crate) async fn send_report_system_turn(
        &self,
        response_json: String,
        structured_json: Option<String>,
    ) -> Result<(), String> {
        self.tx
            .send(TransponderEvent {
                event: Some(transponder_event::Event::ReportSystemTurn(
                    ReportSystemTurn {
                        response_json,
                        structured_json,
                    },
                )),
            })
            .await
            .map_err(|_| "pkm session closed".to_string())
    }

    pub(crate) async fn next_event(&mut self) -> Result<Option<PkmEvent>, String> {
        self.rx
            .message()
            .await
            .map_err(|e| format!("pkm stream error: {e}"))
    }
}

pub(crate) struct AirlockClient {
    inner: AirlockControllerClient<AuthenticatedChannel>,
}

impl AirlockClient {
    pub(crate) async fn connect(addr: &str) -> Result<Self, String> {
        for attempt in 1..=10 {
            let result = Channel::from_shared(addr.to_string())
                .map_err(|e| format!("invalid endpoint: {e}"))?
                .connect()
                .await;
            match result {
                Ok(channel) => {
                    let inner =
                        AirlockControllerClient::with_interceptor(channel, SaTokenInterceptor);
                    return Ok(Self { inner });
                }
                Err(e) if attempt < 10 => {
                    tracing::warn!(attempt, addr, error = %e, "retrying airlock connection");
                    tokio::time::sleep(std::time::Duration::from_secs(attempt)).await;
                }
                Err(e) => {
                    return Err(format!("failed to connect to airlock at {addr}: {e}"));
                }
            }
        }
        unreachable!()
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
