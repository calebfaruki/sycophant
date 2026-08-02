#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().json().with_target(false).init();
    harness::run().await
}
