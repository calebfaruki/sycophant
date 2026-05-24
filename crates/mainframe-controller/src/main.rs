use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use mainframe_controller::materialize::MaterializationContext;
use mainframe_controller::{state, watcher};
use tonic::transport::Server;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "mainframe-controller", version)]
struct Args {
    /// gRPC listen port (health-only).
    #[arg(long, default_value = "9090")]
    port: u16,

    /// Kubernetes namespace to watch for Kernel and Workspace CRDs.
    #[arg(long, default_value = "default")]
    namespace: String,

    /// Periodic reconcile cadence in seconds. Each tick re-reconciles every
    /// known Kernel (no-op) and Workspace (SSA-reapplies the four child
    /// resources idempotently). Lower values make changes propagate
    /// faster after a controller restart; higher values reduce API
    /// server load.
    #[arg(long, default_value = "60")]
    refresh_interval_seconds: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().json().with_target(false).init();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args = Args::parse();

    let kube_client = shared::try_init_kube_client()
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    let ctx = Arc::new(
        MaterializationContext::from_env()
            .map_err(|e| anyhow::anyhow!("materialization context: {e}"))?,
    );

    let state = state::ControllerState::new();

    let addr: SocketAddr = ([0, 0, 0, 0], args.port).into();
    info!(
        %addr,
        namespace = %args.namespace,
        release = %ctx.release_name,
        transponder_image = %ctx.transponder_image,
        transponder_tag = %ctx.transponder_tag,
        "starting mainframe-controller"
    );

    let (kernel_ready_tx, mut kernel_ready_rx) = tokio::sync::watch::channel(false);
    let (workspace_ready_tx, mut workspace_ready_rx) = tokio::sync::watch::channel(false);

    let kernel_namespace = args.namespace.clone();
    let kernel_state = state.clone();
    let kernel_client = kube_client.clone();
    let kernel_watcher_handle = shared::watcher_retry::spawn_watcher_task("kernels", move || {
        let ns = kernel_namespace.clone();
        let client = kernel_client.clone();
        let state = kernel_state.clone();
        let tx = kernel_ready_tx.clone();
        async move { watcher::watch_kernels(client, &ns, state, tx).await }
    });

    let workspace_namespace = args.namespace.clone();
    let workspace_state = state.clone();
    let workspace_client = kube_client.clone();
    let workspace_ctx = ctx.clone();
    let workspace_watcher_handle =
        shared::watcher_retry::spawn_watcher_task("workspaces", move || {
            let ns = workspace_namespace.clone();
            let client = workspace_client.clone();
            let state = workspace_state.clone();
            let ctx = workspace_ctx.clone();
            let tx = workspace_ready_tx.clone();
            async move { watcher::watch_workspaces(client, &ns, state, ctx, tx).await }
        });

    let refresh_namespace = args.namespace.clone();
    let refresh_state = state.clone();
    let refresh_client = kube_client.clone();
    let refresh_ctx = ctx.clone();
    let refresh_interval = args.refresh_interval_seconds;
    let refresh_handle = tokio::spawn(async move {
        watcher::refresh_loop(
            refresh_client,
            refresh_namespace,
            refresh_state,
            refresh_ctx,
            refresh_interval,
        )
        .await
    });

    let grpc_handle = tokio::spawn(async move {
        match tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(
                async {
                    let _ = kernel_ready_rx.wait_for(|&v| v).await;
                },
                async {
                    let _ = workspace_ready_rx.wait_for(|&v| v).await;
                },
            );
        })
        .await
        {
            Ok(_) => info!("watchers initial sync complete, starting gRPC server"),
            Err(_) => warn!("watcher sync timed out after 10s, starting gRPC server"),
        }

        let (_health_reporter, health_service) = tonic_health::server::health_reporter();

        Server::builder()
            .add_service(health_service)
            .serve(addr)
            .await
    });

    tokio::select! {
        result = grpc_handle => {
            error!("gRPC server exited: {:?}", result);
        }
        result = kernel_watcher_handle => {
            error!("kernel watcher exited: {:?}", result);
        }
        result = workspace_watcher_handle => {
            error!("workspace watcher exited: {:?}", result);
        }
        _ = refresh_handle => {
            error!("refresh loop exited");
        }
    }

    Ok(())
}
