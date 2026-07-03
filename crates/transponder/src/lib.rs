mod agent;
mod channel_tools;
mod clients;
mod config;
// `conversation` is the workspace-history library surface (log store,
// scopes, frontmatter, snapshots). Exposed as a public module so its
// intentionally broad API — exercised by unit tests and reserved for the
// delegate-scoping path — isn't dead-code-flagged in this bin-shaped crate.
pub mod conversation;
mod grpc_server;
mod healthz;
mod message_source;
mod registry;
mod runtime_entrypoint;
mod runtime_tools;
mod tool_router;
mod turn;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use config::TransponderConfig;
use conversation::{ConversationStoreFactory, LocalFsFactory};
use message_source::MessageSource;
use registry::ConversationRegistry;
use tokio::sync::Mutex;
use tonic::transport::Server;

/// Default on-disk root for conversation event logs. Mounted from the
/// per-workspace conversation PVC by the chart.
const DEFAULT_CONVERSATION_DIR: &str = "/var/lib/transponder/conversations";

const HEALTHZ_PORT: u16 = 8080;
/// Inbound gRPC port for hangar → transponder forwarding (WatchTools,
/// CallTool). Mirrors the airlock/mainframe controller pattern.
const GRPC_PORT: u16 = 9090;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = TransponderConfig::from_env().map_err(|e| format!("config error: {e}"))?;

    // Conversation log store + registry. The transponder is the sole author
    // of its workspace's history; rebuild the in-memory registry from the
    // PVC at boot so restart preserves thread context.
    let conversation_dir = std::env::var("TRANSPONDER_CONVERSATION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONVERSATION_DIR));
    let factory: Arc<dyn ConversationStoreFactory> =
        Arc::new(LocalFsFactory::new(conversation_dir.clone()));
    let registry = Arc::new(ConversationRegistry::new(factory));
    if let Err(e) = registry.rebuild_from_disk().await {
        tracing::warn!(error = %e, "conversation registry rebuild failed; continuing empty");
    }
    tracing::info!(dir = %conversation_dir.display(), "conversation store ready");

    let mut hangar = clients::HangarClient::connect(&config.hangar_addr).await?;
    let hangar_for_grpc = hangar.clone();
    tracing::info!(addr = %config.hangar_addr, "connected to hangar controller");

    let tightbeam = clients::TightbeamClient::connect(&config.tightbeam_gateway_addr).await?;
    let tightbeam_subscribe = tightbeam.clone();
    let mut tightbeam_deliver = tightbeam.clone();
    tracing::info!(addr = %config.tightbeam_gateway_addr, "connected to tightbeam gateway");

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

    let tool_router = Arc::new(tool_router::ToolRouter::new(
        Some(mainframe_for_router),
        airlock_for_router,
        Some(tightbeam),
        registry.clone(),
    ));

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

    // Start the inbound gRPC server for hangar-controller forwards.
    // Authentication is structural in v1: NetworkPolicy restricts
    // ingress to hangar-ctrl. TokenReview-based audience check is a
    // follow-up; the audience constant + chart mounts are in place.
    let grpc_router_handle = tool_router.clone();
    let grpc_registry_handle = registry.clone();
    let grpc_addr: SocketAddr = ([0, 0, 0, 0], GRPC_PORT).into();
    tokio::spawn(async move {
        let svc = grpc_server::TransponderService::new(
            grpc_router_handle,
            hangar_for_grpc,
            grpc_registry_handle,
        );
        let server = Server::builder()
            .add_service(
                hangar_proto::transponder_control_server::TransponderControlServer::new(svc),
            )
            .serve(grpc_addr);
        if let Err(e) = server.await {
            tracing::error!(error = %e, "transponder gRPC server exited");
        }
    });
    tracing::info!(addr = %grpc_addr, "transponder gRPC server listening");

    let mut source: Box<dyn MessageSource> = Box::new(
        message_source::SubscribeMessageSource::from_client(tightbeam_subscribe, subscribed_flag),
    );

    runtime_entrypoint::message_loop(
        config.max_iterations,
        std::time::Duration::from_secs(config.idle_gap_secs),
        &mut hangar,
        &mut tightbeam_deliver,
        agent_cache,
        tool_router,
        registry,
        source.as_mut(),
    )
    .await?;

    Ok(())
}
