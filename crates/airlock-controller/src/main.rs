use std::net::SocketAddr;

use airlock_controller::{grpc, keepalive, state, watcher};

use airlock_proto::airlock_controller_server::AirlockControllerServer;
use clap::Parser;
use tonic::transport::Server;
use tracing::{error, info};

#[derive(Parser)]
#[command(name = "airlock-controller", version)]
struct Args {
    /// gRPC listen port.
    #[arg(long, default_value = "9090")]
    port: u16,

    /// Kubernetes namespace to watch for Chamber CRDs.
    #[arg(long, default_value = "default")]
    namespace: String,

    /// Reachable address for Jobs to connect back to this controller.
    /// Defaults to http://0.0.0.0:{port} which only works when Jobs
    /// run on the same host. Set to the Kubernetes Service address
    /// (e.g. http://airlock-ctrl:9090) in cluster deployments.
    #[arg(long)]
    controller_addr: Option<String>,

    /// Path to the workspace-to-chambers bindings YAML file.
    #[arg(long)]
    bindings_file: Option<String>,

    /// Path to the scheduling config YAML file (nodeSelector + tolerations for Jobs).
    #[arg(long, default_value = "/etc/sycophant/scheduling.yaml")]
    scheduling_file: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().json().with_target(false).init();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args = Args::parse();

    let kube_client = shared::try_init_kube_client()
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    let controller_addr = args
        .controller_addr
        .unwrap_or_else(|| format!("http://0.0.0.0:{}", args.port));

    let scheduling =
        shared::scheduling::SchedulingConfig::load_or_default(&args.scheduling_file, true)
            .map_err(|e| anyhow::anyhow!(e))?;

    let state = state::ControllerState::new(
        Some(kube_client.clone()),
        args.namespace.clone(),
        controller_addr,
        scheduling,
    );

    let verifier: Option<std::sync::Arc<dyn shared::auth::TokenVerifier>> =
        Some(std::sync::Arc::new(shared::auth::K8sTokenVerifier::new(
            kube_client.clone(),
            shared::auth::HARNESS_AIRLOCK_AUDIENCE,
        )) as _);

    let bindings = match &args.bindings_file {
        Some(path) if std::path::Path::new(path).exists() => {
            match state::WorkspaceBindings::load(path) {
                Ok(b) => {
                    info!(path = %path, "loaded workspace bindings");
                    b
                }
                Err(e) => {
                    error!(path = %path, error = %e, "failed to load bindings, using empty");
                    state::WorkspaceBindings::empty()
                }
            }
        }
        _ => {
            info!("no bindings file, workspace scoping disabled");
            state::WorkspaceBindings::empty()
        }
    };

    let addr: SocketAddr = ([0, 0, 0, 0], args.port).into();
    info!(%addr, namespace = %args.namespace, "starting airlock-controller");

    let (chamber_ready_tx, chamber_ready_rx) = tokio::sync::watch::channel(false);

    let chamber_watcher_ns = args.namespace.clone();
    let chamber_watcher_state = state.clone();
    let chamber_watcher_client = kube_client.clone();
    let chamber_watcher_handle = shared::watcher_retry::spawn_watcher_task("chambers", move || {
        let ns = chamber_watcher_ns.clone();
        let client = chamber_watcher_client.clone();
        let state = chamber_watcher_state.clone();
        let tx = chamber_ready_tx.clone();
        async move { watcher::watch_chambers(client, &ns, state, tx).await }
    });

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_not_serving::<AirlockControllerServer<grpc::ControllerService>>()
        .await;

    let health_for_watch = health_reporter.clone();
    let mut health_ready_rx = chamber_ready_rx.clone();
    tokio::spawn(async move {
        while health_ready_rx.changed().await.is_ok() {
            let healthy = *health_ready_rx.borrow();
            if healthy {
                health_for_watch
                    .set_serving::<AirlockControllerServer<grpc::ControllerService>>()
                    .await;
                info!("readiness: all chambers registered, serving");
            } else {
                health_for_watch
                    .set_not_serving::<AirlockControllerServer<grpc::ControllerService>>()
                    .await;
                info!("readiness: chamber(s) failed discovery, NOT serving");
            }
        }
    });

    let grpc_state = state.clone();
    let grpc_verifier = verifier;
    let grpc_bindings = bindings;
    let grpc_handle = tokio::spawn(async move {
        let svc = grpc::ControllerService::new(grpc_state, grpc_verifier, grpc_bindings);
        Server::builder()
            .add_service(health_service)
            .add_service(AirlockControllerServer::new(svc))
            .serve(addr)
            .await
    });

    // Reconcile existing Jobs into the in-memory active_jobs map. Without
    // this, after an airlock-ctrl restart the next CallTool would see
    // `get_active_job=None`, spawn a duplicate Job, and leak the old
    // pod. Wait for chamber_ready to flip true so `state.get_chamber()`
    // returns populated specs (resolves per-chamber keepalive flag).
    let reconcile_state = state.clone();
    let reconcile_client = kube_client.clone();
    let reconcile_ns = args.namespace.clone();
    let mut reconcile_rx = chamber_ready_rx.clone();
    tokio::spawn(async move {
        loop {
            if *reconcile_rx.borrow() {
                break;
            }
            if reconcile_rx.changed().await.is_err() {
                return;
            }
        }
        if let Err(e) =
            keepalive::reconcile_active_jobs(&reconcile_client, &reconcile_ns, &reconcile_state)
                .await
        {
            error!(error = %e, "reconcile_active_jobs failed; cleanup loop will operate on partial state");
        }
    });

    // Reactive tool-Job watch: fail a parked call the instant its chamber
    // Job goes terminal or is deleted, rather than waiting for the idle
    // sweep. Existing batch/jobs:watch RBAC — no new permission.
    let tooljob_watch_state = state.clone();
    let tooljob_watch_client = kube_client.clone();
    let tooljob_watch_ns = args.namespace.clone();
    shared::watcher_retry::spawn_watcher_task("tool-jobs", move || {
        let client = tooljob_watch_client.clone();
        let ns = tooljob_watch_ns.clone();
        let state = tooljob_watch_state.clone();
        async move { keepalive::watch_tool_jobs(client, &ns, state).await }
    });

    let keepalive_handle = tokio::spawn(keepalive::cleanup_loop(state));

    tokio::select! {
        result = grpc_handle => {
            error!("gRPC server exited: {:?}", result);
        }
        result = chamber_watcher_handle => {
            error!("chamber watcher exited: {:?}", result);
        }
        _ = keepalive_handle => {
            error!("keepalive cleanup task exited");
        }
    }

    Ok(())
}
