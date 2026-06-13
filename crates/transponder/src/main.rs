mod agent;
mod clients;
mod config;
mod healthz;
mod message_source;
mod runtime_entrypoint;
mod runtime_tools;
mod tool_router;
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

    // Three MainframeClient handles share a single underlying HTTP/2
    // connection: router (call_tool), tool watcher (watch_tools), and
    // agent watcher (per-30s get_agent for the persona cache).
    let (mainframe_for_router, mainframe_for_tool_watch, mainframe_for_agent_watch) = {
        let addr = &config.mainframe_addr;
        let client = clients::MainframeClient::connect(addr).await?;
        tracing::info!(addr = %addr, "connected to mainframe controller");
        (client.clone(), client.clone(), client)
    };

    let tool_router = tool_router::ToolRouter::new(Some(mainframe_for_router), airlock_for_router);
    let tool_router = Arc::new(Mutex::new(tool_router));

    // Shared persona cache. Empty until `watch_mainframe_agent` lands its
    // first refresh; the initial_waits barrier below blocks message
    // processing until that happens.
    let agent_cache: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    let mut initial_waits = Vec::new();

    if let Some(watch_client) = airlock_for_watch {
        let router_for_watch = tool_router.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        initial_waits.push(rx);
        tokio::spawn(async move {
            tool_router::watch_airlock_tools(watch_client, router_for_watch, Some(tx)).await;
        });
    }

    {
        let router_for_watch = tool_router.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        initial_waits.push(rx);
        tokio::spawn(async move {
            tool_router::watch_mainframe_tools(
                mainframe_for_tool_watch,
                router_for_watch,
                Some(tx),
            )
            .await;
        });
    }

    {
        let cache_for_watch = agent_cache.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        initial_waits.push(rx);
        tokio::spawn(async move {
            runtime_entrypoint::watch_mainframe_agent(
                mainframe_for_agent_watch,
                cache_for_watch,
                Some(tx),
            )
            .await;
        });
    }

    // Block message processing until every wired source delivers its
    // first snapshot — tool sets AND the primary persona cache.
    for rx in initial_waits {
        let _ = rx.await;
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

    runtime_entrypoint::message_loop(
        config.max_iterations,
        &mut tightbeam,
        agent_cache,
        tool_router,
        source.as_mut(),
    )
    .await?;

    Ok(())
}
