mod server;
mod tools;

use std::path::PathBuf;

use airlock_proto::airlock_controller_server::AirlockControllerServer;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;

fn socket_path() -> PathBuf {
    std::env::var("MAINFRAME_RUNTIME_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/mainframe/runtime.sock"))
}

fn max_output_chars() -> usize {
    std::env::var("MAX_OUTPUT_CHARS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().json().with_target(false).init();

    let path = socket_path();
    let max_chars = max_output_chars();

    // Remove stale socket file if it exists
    let _ = std::fs::remove_file(&path);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let uds = UnixListener::bind(&path)?;
    let uds_stream = UnixListenerStream::new(uds);

    tracing::info!(socket = %path.display(), "mainframe-runtime listening");

    let service = server::MainframeRuntimeService::new(max_chars);

    tonic::transport::Server::builder()
        .add_service(AirlockControllerServer::new(service))
        .serve_with_incoming(uds_stream)
        .await?;

    Ok(())
}
