#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().json().with_target(false).init();
    // Pin the rustls 0.23 CryptoProvider; it refuses to auto-pick with multiple
    // compiled in.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    harness::run().await
}
