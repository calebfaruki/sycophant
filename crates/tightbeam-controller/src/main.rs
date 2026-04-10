use std::sync::Arc;
use tightbeam_controller::conversation::ConversationLog;
use tightbeam_controller::grpc::ControllerService;
use tightbeam_controller::state::ControllerState;
use tightbeam_proto::tightbeam_controller_server::TightbeamControllerServer;
use tonic::transport::Server;

const DEFAULT_LOG_DIR: &str = "/var/log/tightbeam";
const DEFAULT_GRPC_PORT: u16 = 9090;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let log_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_LOG_DIR.into());

    let conversation = match ConversationLog::rebuild(std::path::Path::new(&log_dir)) {
        Ok(c) => {
            let count = c.history().len();
            if count > 0 {
                tracing::info!("rebuilt {count} messages from conversation log");
            }
            c
        }
        Err(e) => {
            tracing::warn!("failed to rebuild conversation: {e}, starting fresh");
            ConversationLog::new(std::path::Path::new(&log_dir))
        }
    };

    let kube_client = kube::Client::try_default().await.ok();
    if kube_client.is_some() {
        tracing::info!("k8s client initialized, auto Job creation enabled");
    } else {
        tracing::info!("no k8s client available, auto Job creation disabled");
    }

    let namespace = std::env::var("TIGHTBEAM_NAMESPACE").unwrap_or_else(|_| "default".into());
    let controller_addr = std::env::var("TIGHTBEAM_CONTROLLER_ADDR")
        .unwrap_or_else(|_| format!("http://0.0.0.0:{DEFAULT_GRPC_PORT}"));
    let llm_job_image = std::env::var("TIGHTBEAM_LLM_JOB_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/calebfaruki/tightbeam-llm-job:latest".into());

    let state = Arc::new(ControllerState::new(
        conversation,
        kube_client.clone(),
        namespace.clone(),
        controller_addr,
        llm_job_image,
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

    let service = ControllerService::new(state);

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
