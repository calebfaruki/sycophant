use shared::auth::K8sTokenVerifier;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tightbeam_controller::conversation::ConversationLog;
use tightbeam_controller::grpc::ControllerService;
use tightbeam_controller::state::ControllerState;
use tightbeam_proto::tightbeam_controller_server::TightbeamControllerServer;
use tonic::transport::Server;

const DEFAULT_LOG_DIR: &str = "/var/log/tightbeam";
const DEFAULT_GRPC_PORT: u16 = 9090;

fn scan_workspace_convs(log_dir: &Path) -> HashMap<String, ConversationLog> {
    let mut convs = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                match ConversationLog::rebuild(&path) {
                    Ok(conv) => {
                        let count = conv.len();
                        if count > 0 {
                            tracing::info!(
                                workspace = %name,
                                "rebuilt {count} messages from conversation log"
                            );
                        }
                        convs.insert(name, conv);
                    }
                    Err(e) => {
                        tracing::warn!(
                            workspace = %name,
                            "failed to rebuild conversation: {e}, starting fresh"
                        );
                        convs.insert(name, ConversationLog::new(&path));
                    }
                }
            }
        }
    }
    convs
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let log_dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| DEFAULT_LOG_DIR.into()),
    );

    let workspace_convs = scan_workspace_convs(&log_dir);
    if workspace_convs.is_empty() {
        tracing::info!("no existing workspace logs found");
    } else {
        tracing::info!("loaded {} workspace(s) from disk", workspace_convs.len());
    }

    let kube_client = shared::try_init_kube_client().await?;

    let verifier = kube_client.as_ref().map(|c| {
        Arc::new(K8sTokenVerifier::new(c.clone())) as Arc<dyn shared::auth::TokenVerifier>
    });

    let namespace = std::env::var("TIGHTBEAM_NAMESPACE").unwrap_or_else(|_| "default".into());
    let controller_addr = std::env::var("TIGHTBEAM_CONTROLLER_ADDR")
        .unwrap_or_else(|_| format!("http://0.0.0.0:{DEFAULT_GRPC_PORT}"));
    let llm_job_image = std::env::var("TIGHTBEAM_LLM_JOB_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/calebfaruki/tightbeam-llm-job:latest".into());

    let scheduling_file = std::env::var("TIGHTBEAM_SCHEDULING_FILE")
        .unwrap_or_else(|_| "/etc/sycophant/scheduling.yaml".into());
    let scheduling = shared::scheduling::SchedulingConfig::load_or_default(
        &scheduling_file,
        kube_client.is_some(),
    )?;

    let state = Arc::new(ControllerState::new(
        workspace_convs,
        log_dir,
        kube_client.clone(),
        namespace.clone(),
        controller_addr,
        llm_job_image,
        scheduling,
    ));

    if kube_client.is_some() {
        let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(false);

        let watcher_state = state.clone();
        let watcher_ns = namespace;
        tokio::spawn(async move {
            // Separate kube client for the watcher to avoid HTTP/2
            // connection multiplexing issues with the Job creation client.
            let client = match kube::Client::try_default().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("watcher kube client failed: {e}");
                    return;
                }
            };
            if let Err(e) = tightbeam_controller::watcher::watch_models(
                client,
                &watcher_ns,
                watcher_state,
                ready_tx,
            )
            .await
            {
                tracing::error!("model watcher failed: {e}");
            }
        });

        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            ready_rx.wait_for(|&v| v),
        )
        .await
        {
            Ok(Ok(_)) => tracing::info!("watcher initial sync complete"),
            Ok(Err(_)) => tracing::warn!("watcher channel closed before sync"),
            Err(_) => tracing::warn!("watcher sync timed out after 10s, serving anyway"),
        };
    }

    let service = ControllerService::new(state, verifier);

    let addr = format!("0.0.0.0:{DEFAULT_GRPC_PORT}").parse()?;
    tracing::info!("tightbeam-controller listening on {addr}");

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<TightbeamControllerServer<ControllerService>>()
        .await;

    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(tightbeam_proto::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    Server::builder()
        .add_service(reflection_service)
        .add_service(health_service)
        .add_service(TightbeamControllerServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
