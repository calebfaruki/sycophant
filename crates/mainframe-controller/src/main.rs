use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use mainframe_controller::{grpc, kernel::Kernel, state, watcher};
use mainframe_proto::mainframe_controller_server::MainframeControllerServer;
use tonic::transport::Server;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "mainframe-controller", version)]
struct Args {
    /// gRPC listen port.
    #[arg(long, default_value = "9090")]
    port: u16,

    /// Kubernetes namespace to watch for Kernel CRDs.
    #[arg(long, default_value = "default")]
    namespace: String,

    /// Periodic reconcile cadence in seconds for the Kernel watcher.
    #[arg(long, default_value = "60")]
    refresh_interval_seconds: u64,

    /// Root path under which per-workspace kernel directories live. The
    /// chart mounts `<kernels_root>/<workspace>/` for each Workspace CR.
    #[arg(long, default_value = "/etc/kernels")]
    kernels_root: std::path::PathBuf,
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

    let verifier: Option<Arc<dyn shared::auth::TokenVerifier>> =
        Some(Arc::new(shared::auth::K8sTokenVerifier::new(
            kube_client.clone(),
            shared::auth::TRANSPONDER_MAINFRAME_AUDIENCE,
        )) as _);

    let kernel = Arc::new(Kernel::new(args.kernels_root.clone()));
    info!(kernels_root = %args.kernels_root.display(), "kernel root configured");

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_not_serving::<MainframeControllerServer<grpc::ControllerService>>()
        .await;

    let health_for_watch = health_reporter.clone();
    let readiness_state = state.clone();
    let readiness_root = args.kernels_root.clone();
    let grpc_handle = tokio::spawn(async move {
        match tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let _ = kernel_ready_rx.wait_for(|&v| v).await;
        })
        .await
        {
            Ok(_) => {
                // Kernel CRs are now visible in the apiserver, but the per-
                // workspace directories may not be mounted yet (HostPath
                // attach race, S3 init container still syncing). Probe the
                // filesystem before reporting Ready so the apiserver doesn't
                // route requests we can't actually serve.
                let names = readiness_state.list_kernel_names().await;
                let mut any_ready = false;
                for name in &names {
                    let path = readiness_root.join(name);
                    if tokio::fs::read_dir(&path).await.is_ok() {
                        any_ready = true;
                        info!(workspace = %name, "workspace kernel directory readable");
                        break;
                    } else {
                        warn!(workspace = %name, path = %path.display(), "kernel directory not yet readable");
                    }
                }
                if any_ready {
                    health_for_watch
                        .set_serving::<MainframeControllerServer<grpc::ControllerService>>()
                        .await;
                    info!("at least one workspace kernel mounted, serving");
                } else {
                    warn!(
                        workspace_count = names.len(),
                        "no workspace kernel directories readable; NOT serving"
                    );
                }
            }
            Err(_) => {
                warn!("kernel watcher sync timed out after 10s, NOT serving");
            }
        }

        let svc = grpc::ControllerService::new(kernel, verifier);
        Server::builder()
            .add_service(health_service)
            .add_service(MainframeControllerServer::new(svc))
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
