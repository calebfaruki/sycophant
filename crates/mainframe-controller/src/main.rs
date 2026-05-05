use std::net::SocketAddr;

use clap::Parser;
use mainframe_controller::{state, watcher};
use tonic::transport::Server;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "mainframe-controller", version)]
struct Args {
    /// gRPC listen port (health-only).
    #[arg(long, default_value = "9090")]
    port: u16,

    /// Kubernetes namespace to watch for Mainframe CRDs.
    #[arg(long, default_value = "default")]
    namespace: String,

    /// Filesystem path where pulled S3 contents are written. The chart mounts
    /// the controller-owned PVC here. Workspace pods mount the same PVC
    /// read-only at their `/etc/mainframe`.
    #[arg(long, default_value = "/data/mainframe")]
    data_dir: String,

    /// Periodic reconcile cadence in seconds. Each tick re-reconciles every
    /// known Mainframe.
    #[arg(long, default_value = "60")]
    refresh_interval_seconds: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().json().with_target(false).init();

    let args = Args::parse();

    let kube_client = shared::try_init_kube_client()
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    if let Err(e) = std::fs::create_dir_all(&args.data_dir) {
        error!(path = %args.data_dir, error = %e, "failed to create data dir");
        return Err(anyhow::anyhow!(e));
    }

    let state = state::ControllerState::new(
        kube_client.clone(),
        args.namespace.clone(),
        args.data_dir.clone(),
    );

    let addr: SocketAddr = ([0, 0, 0, 0], args.port).into();
    info!(%addr, namespace = %args.namespace, data_dir = %args.data_dir, "starting mainframe-controller");

    let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(false);

    let watcher_namespace = args.namespace.clone();
    let watcher_state = state.clone();
    let watcher_client = kube_client.clone();
    let watcher_handle = tokio::spawn(async move {
        let client = match watcher_client {
            Some(c) => c,
            None => match kube::Client::try_default().await {
                Ok(c) => c,
                Err(e) => {
                    error!("watcher kube client failed: {e}");
                    return Ok(());
                }
            },
        };
        watcher::watch_mainframes(client, &watcher_namespace, watcher_state, ready_tx).await
    });

    let refresh_namespace = args.namespace.clone();
    let refresh_state = state.clone();
    let refresh_client = kube_client.clone();
    let refresh_interval = args.refresh_interval_seconds;
    let refresh_handle = tokio::spawn(async move {
        let client = match refresh_client {
            Some(c) => c,
            None => match kube::Client::try_default().await {
                Ok(c) => c,
                Err(e) => {
                    error!("refresh kube client failed: {e}");
                    return;
                }
            },
        };
        watcher::refresh_loop(client, refresh_namespace, refresh_state, refresh_interval).await
    });

    let grpc_handle = tokio::spawn(async move {
        match tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let _ = ready_rx.wait_for(|&v| v).await;
        })
        .await
        {
            Ok(()) => info!("watcher initial sync complete, starting gRPC server"),
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
        result = watcher_handle => {
            error!("mainframe watcher exited: {:?}", result);
        }
        _ = refresh_handle => {
            error!("refresh loop exited");
        }
    }

    Ok(())
}
