mod server;
mod tools;

use std::net::SocketAddr;

use airlock_proto::airlock_controller_server::AirlockControllerServer;

const LISTEN_ADDR: &str = "127.0.0.1:50051";

fn max_output_chars() -> usize {
    std::env::var("MAX_OUTPUT_CHARS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().json().with_target(false).init();

    let max_chars = max_output_chars();
    let addr: SocketAddr = LISTEN_ADDR.parse()?;

    let service = server::MainframeRuntimeService::new(max_chars);

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<AirlockControllerServer<server::MainframeRuntimeService>>()
        .await;

    tracing::info!(%addr, "mainframe-runtime listening");

    tonic::transport::Server::builder()
        .add_service(health_service)
        .add_service(AirlockControllerServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
