mod agent;
mod clients;
mod config;
mod message_source;
mod runtime_entrypoint;
mod tool_router;
mod transponder_tools;
mod turn;

use config::TransponderConfig;
use message_source::MessageSource;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().json().with_target(false).init();

    let config = TransponderConfig::from_env().map_err(|e| format!("config error: {e}"))?;

    let mut tightbeam = clients::TightbeamClient::connect(&config.tightbeam_addr).await?;
    let mut tightbeam_subscribe = clients::TightbeamClient::connect(&config.tightbeam_addr).await?;
    tracing::info!(addr = %config.tightbeam_addr, "connected to tightbeam controller");

    let workspace = clients::ToolClient::connect_uds(&config.workspace_tools_socket).await?;
    tracing::info!(socket = %config.workspace_tools_socket.display(), "connected to workspace tools");

    let airlock = match &config.airlock_addr {
        Some(addr) => {
            let client = clients::AirlockClient::connect(addr).await?;
            tracing::info!(addr = %addr, "connected to airlock controller");
            Some(client)
        }
        None => None,
    };

    let mut tool_router = tool_router::ToolRouter::new(airlock, workspace);
    tool_router.initialize().await?;

    let mut source: Box<dyn MessageSource> = if config.use_stdin {
        tracing::info!("using stdin message source");
        Box::new(message_source::StdinMessageSource::new())
    } else {
        let stream = tightbeam_subscribe.subscribe().await?;
        tracing::info!("subscribed to tightbeam for inbound messages");
        Box::new(message_source::SubscribeMessageSource::new(stream))
    };

    runtime_entrypoint::run(
        config.max_iterations,
        &mut tightbeam,
        &mut tool_router,
        source.as_mut(),
        config.entrypoint_path,
    )
    .await?;

    Ok(())
}
