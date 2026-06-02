mod agent;
mod clients;
mod config;
mod healthz;
mod message_source;
mod runtime_entrypoint;
mod tool_router;
mod transponder_tools;
mod turn;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use config::TransponderConfig;
use message_source::MessageSource;
use tokio::sync::Mutex;

const HEALTHZ_PORT: u16 = 8080;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().json().with_target(false).init();

    let config = TransponderConfig::from_env().map_err(|e| format!("config error: {e}"))?;

    let mut tightbeam = clients::TightbeamClient::connect(&config.tightbeam_addr).await?;
    let tightbeam_subscribe = clients::TightbeamClient::connect(&config.tightbeam_addr).await?;
    tracing::info!(addr = %config.tightbeam_addr, "connected to tightbeam controller");

    // Two AirlockClient handles share a single underlying HTTP/2 connection
    // (tonic Channels multiplex). One handle is held by the router for
    // `call_tool` (needs `&mut self`); the other is moved into the background
    // `watch_airlock_tools` task. The Rust borrow constraint requires two
    // distinct values; the network sees one connection.
    let (airlock_for_router, airlock_for_watch) = match &config.airlock_addr {
        Some(addr) => {
            let client = clients::AirlockClient::connect(addr).await?;
            tracing::info!(addr = %addr, "connected to airlock controller");
            (Some(client.clone()), Some(client))
        }
        None => (None, None),
    };

    let tool_router = tool_router::ToolRouter::new(airlock_for_router);
    let tool_router = Arc::new(Mutex::new(tool_router));

    if let Some(watch_client) = airlock_for_watch {
        let router_for_watch = tool_router.clone();
        let (initial_tx, initial_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            tool_router::watch_airlock_tools(watch_client, router_for_watch, Some(initial_tx))
                .await;
        });
        // Block message processing until the watch task delivers its first
        // snapshot. Avoids a startup race where an inbound user message
        // arrives before any airlock tools are loaded into the router.
        let _ = initial_rx.await;
    }

    let subscribed_flag = Arc::new(AtomicBool::new(false));
    tokio::spawn(healthz::serve(subscribed_flag.clone(), HEALTHZ_PORT));

    let mut source: Box<dyn MessageSource> = if config.use_stdin {
        tracing::info!("using stdin message source");
        Box::new(message_source::StdinMessageSource::new())
    } else {
        Box::new(message_source::SubscribeMessageSource::from_client(
            tightbeam_subscribe,
            subscribed_flag,
        ))
    };

    runtime_entrypoint::run(
        config.max_iterations,
        &mut tightbeam,
        tool_router,
        source.as_mut(),
    )
    .await?;

    Ok(())
}
