use shared::auth::SaTokenInterceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use toolset_proto::toolset_controller_client::ToolsetControllerClient;

/// Dial the toolset controller over a keepalive channel and wrap it with
/// `SaTokenInterceptor`, so every RPC (GetToolCall / StreamToolResult /
/// AwaitToolCancel) carries the pod's projected SA token as a Bearer header.
pub async fn connect_authenticated(
    controller_addr: &str,
    interceptor: SaTokenInterceptor,
) -> anyhow::Result<ToolsetControllerClient<InterceptedService<Channel, SaTokenInterceptor>>> {
    let channel =
        shared::grpc_client::connect_with_keepalive(controller_addr, "toolset-controller")
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
    Ok(ToolsetControllerClient::with_interceptor(
        channel,
        interceptor,
    ))
}
