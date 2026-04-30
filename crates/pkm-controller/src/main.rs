use std::path::PathBuf;
use std::sync::Arc;

use pkm_controller::grpc::PkmServiceImpl;
use pkm_controller::state::PkmState;
use pkm_proto::pkm_service_server::PkmServiceServer;
use pkm_proto::FILE_DESCRIPTOR_SET;
use sycophant_auth::{K8sTokenVerifier, TokenVerifier};
use tonic::transport::Server;

const DEFAULT_GRPC_PORT: u16 = 9090;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().json().with_target(false).init();

    let pkm_dir: PathBuf = std::env::var("PKM_DIR")
        .map_err(|_| "PKM_DIR not set")?
        .into();

    let grpc_addr =
        std::env::var("PKM_GRPC_ADDR").unwrap_or_else(|_| format!("0.0.0.0:{DEFAULT_GRPC_PORT}"));

    let state = Arc::new(PkmState::new(&pkm_dir).await?);
    tracing::info!(
        agents = state.prompts().len(),
        pkm_dir = %pkm_dir.display(),
        "loaded pkm"
    );

    let verifier: Option<Arc<dyn TokenVerifier>> = match kube::Client::try_default().await {
        Ok(client) => {
            tracing::info!("kube client available; enabling token verification");
            Some(Arc::new(K8sTokenVerifier::new(client)))
        }
        Err(e) => {
            tracing::warn!(error = %e, "no kube client; running without auth");
            None
        }
    };

    let service = PkmServiceImpl::new(state, verifier);

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<PkmServiceServer<PkmServiceImpl>>()
        .await;

    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
        .build_v1()?;

    let addr = grpc_addr.parse()?;
    tracing::info!(addr = %grpc_addr, "pkm-controller listening");

    Server::builder()
        .add_service(health_service)
        .add_service(reflection_service)
        .add_service(PkmServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
