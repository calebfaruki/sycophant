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

    /// Kubernetes namespace to watch for Kernel CRDs.
    #[arg(long, default_value = "default")]
    namespace: String,

    /// Periodic reconcile cadence in seconds for the Kernel watcher.
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

    let state = state::ControllerState::new();

    let addr: SocketAddr = ([0, 0, 0, 0], args.port).into();
    info!(
        %addr,
        namespace = %args.namespace,
        "starting mainframe-controller (kernel reconciler only)"
    );

    let (kernel_ready_tx, mut kernel_ready_rx) = tokio::sync::watch::channel(false);

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

    let refresh_namespace = args.namespace.clone();
    let refresh_state = state.clone();
    let refresh_client = kube_client.clone();
    let refresh_interval = args.refresh_interval_seconds;
    let refresh_handle = tokio::spawn(async move {
        watcher::refresh_loop(
            refresh_client,
            refresh_namespace,
            refresh_state,
            refresh_interval,
        )
        .await
    });

    // mainframe-controller exposes no tonic service of its own (pure CRD
    // reconciler). Health is reported on the overall-server name (empty
    // service) — set NotServing until the kernel watcher initial sync
    // completes, then Serving.
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_service_status("", tonic_health::ServingStatus::NotServing)
        .await;

    let health_for_watch = health_reporter.clone();
    let grpc_handle = tokio::spawn(async move {
        match tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let _ = kernel_ready_rx.wait_for(|&v| v).await;
        })
        .await
        {
            Ok(_) => {
                health_for_watch
                    .set_service_status("", tonic_health::ServingStatus::Serving)
                    .await;
                info!("kernel watcher initial sync complete, serving");
            }
            Err(_) => {
                warn!("kernel watcher sync timed out after 10s, NOT serving");
            }
        }

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
        _ = refresh_handle => {
            error!("refresh loop exited");
        }
    }

    Ok(())
}
