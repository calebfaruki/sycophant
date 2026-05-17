use clap::Parser;
use ed25519_dalek::SigningKey;
use shared::auth::K8sTokenVerifier;
use shared::client_signature::ClientSignatureVerifier;
use shared::replay_cache::DEFAULT_WINDOW;
use shared::storage::S3Spec;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tightbeam_controller::conversation::{ConversationStoreFactory, LocalFsFactory, S3Factory};
use tightbeam_controller::grpc::ControllerService;
use tightbeam_controller::signature_layer::SignatureLayer;
use tightbeam_controller::state::ControllerState;
use tightbeam_proto::tightbeam_controller_server::TightbeamControllerServer;
use tonic::transport::Server;

const DEFAULT_LOG_DIR: &str = "/var/log/tightbeam";
/// Internal listener: K8s SA token via TokenReview. Bound `0.0.0.0`
/// so in-cluster workloads (LLM Job, channel adapters, syco-cli pods)
/// can reach it.
const DEFAULT_INTERNAL_GRPC_PORT: u16 = 9090;
/// External listener: signed-request envelope verified by
/// `signature_layer` tower middleware. Bound `127.0.0.1` so only the
/// tsnet-bridge sidecar in the same Pod can route to it.
const DEFAULT_EXTERNAL_GRPC_PORT: u16 = 9091;
/// Default mount path for the signing-key Secret. Override via $TIGHTBEAM_SIGNING_KEY_PATH.
const DEFAULT_SIGNING_KEY_PATH: &str = "/etc/tightbeam/signing-key/key";

#[derive(Parser)]
#[command(
    name = "tightbeam-controller",
    about = "Sycophant tightbeam controller"
)]
struct Cli {
    /// LocalFs conversation event-store directory. Default /var/log/tightbeam.
    #[arg(value_name = "LOG_DIR")]
    log_dir: Option<PathBuf>,
}

/// Load the 32-byte Ed25519 signing key from `path`. Errors if missing or wrong size.
fn load_signing_key(path: &Path) -> std::io::Result<SigningKey> {
    let bytes = std::fs::read(path).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "failed to read signing key at {}: {e}. \
                 The chart's post-install Job creates the `tightbeam-signing-key` \
                 Secret in the release namespace; verify the Secret exists and is \
                 mounted at this path.",
                path.display()
            ),
        )
    })?;
    let bytes_array: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "signing key at {} must be exactly 32 bytes, got {}",
                path.display(),
                bytes.len()
            ),
        )
    })?;
    tracing::info!(path = %path.display(), "loaded signing key");
    Ok(SigningKey::from_bytes(&bytes_array))
}

/// Parse a boolean env var. Only the literal `"true"` is true; anything else is false.
/// `None` (unset) → `default`.
fn parse_bool_env(raw: Option<String>, default: bool) -> bool {
    match raw {
        Some(v) => v == "true",
        None => default,
    }
}

/// Build the auth verifier for the internal gRPC listener. K8s
/// ServiceAccount tokens flow through this verifier; external
/// client-signed requests use `ClientSignatureVerifier` on the
/// separate external listener.
fn build_internal_verifier(
    kube_client: Option<&kube::Client>,
) -> Option<Arc<dyn shared::auth::TokenVerifier>> {
    kube_client
        .map(|c| Arc::new(K8sTokenVerifier::new(c.clone())) as Arc<dyn shared::auth::TokenVerifier>)
}

fn parse_s3_spec_from_env() -> Result<S3Spec, String> {
    let endpoint = std::env::var("TIGHTBEAM_CONVERSATION_SINK_S3_ENDPOINT")
        .map_err(|_| "TIGHTBEAM_CONVERSATION_SINK_S3_ENDPOINT not set".to_string())?;
    let bucket = std::env::var("TIGHTBEAM_CONVERSATION_SINK_S3_BUCKET")
        .map_err(|_| "TIGHTBEAM_CONVERSATION_SINK_S3_BUCKET not set".to_string())?;
    let prefix = std::env::var("TIGHTBEAM_CONVERSATION_SINK_S3_PREFIX")
        .map_err(|_| "TIGHTBEAM_CONVERSATION_SINK_S3_PREFIX not set".to_string())?;
    let region = std::env::var("TIGHTBEAM_CONVERSATION_SINK_S3_REGION")
        .unwrap_or_else(|_| "us-east-1".into());
    let force_path_style = parse_bool_env(
        std::env::var("TIGHTBEAM_CONVERSATION_SINK_S3_FORCE_PATH_STYLE").ok(),
        true,
    );
    Ok(S3Spec {
        endpoint,
        bucket,
        prefix,
        region,
        force_path_style,
        credentials: None,
    })
}

async fn build_conversation_factory(
    log_dir: PathBuf,
) -> Result<Arc<dyn ConversationStoreFactory>, String> {
    let kind =
        std::env::var("TIGHTBEAM_CONVERSATION_SINK_KIND").unwrap_or_else(|_| "LocalFs".into());
    match kind.as_str() {
        "LocalFs" => {
            tracing::info!(log_dir = %log_dir.display(), "conversation sink: LocalFs");
            Ok(Arc::new(LocalFsFactory::new(log_dir)))
        }
        "S3" => {
            let spec = parse_s3_spec_from_env()?;
            let client = shared::storage::build_s3_client(&spec).await;
            tracing::info!(
                endpoint = %spec.endpoint,
                bucket = %spec.bucket,
                prefix = %spec.prefix,
                region = %spec.region,
                force_path_style = spec.force_path_style,
                "conversation sink: S3"
            );
            Ok(Arc::new(S3Factory::new(client, spec)))
        }
        other => Err(format!(
            "TIGHTBEAM_CONVERSATION_SINK_KIND={other} unsupported (expected LocalFs|S3)"
        )),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Pin the rustls 0.23 CryptoProvider; refuses to auto-pick when
    // multiple are compiled in.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();
    let log_dir = cli.log_dir.unwrap_or_else(|| DEFAULT_LOG_DIR.into());

    std::fs::create_dir_all(&log_dir)
        .map_err(|e| format!("failed to create log_dir {}: {e}", log_dir.display()))?;

    let key_path: PathBuf = std::env::var("TIGHTBEAM_SIGNING_KEY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| DEFAULT_SIGNING_KEY_PATH.into());

    let conversation_factory = build_conversation_factory(log_dir.clone()).await?;

    let kube_client = shared::try_init_kube_client().await?;

    let signing_key = load_signing_key(&key_path)?;

    let verifier = build_internal_verifier(Some(&kube_client));

    let namespace = std::env::var("TIGHTBEAM_NAMESPACE").unwrap_or_else(|_| "default".into());
    let controller_addr = std::env::var("TIGHTBEAM_CONTROLLER_ADDR")
        .unwrap_or_else(|_| format!("http://0.0.0.0:{DEFAULT_INTERNAL_GRPC_PORT}"));
    let llm_job_image = std::env::var("TIGHTBEAM_LLM_JOB_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/calebfaruki/tightbeam-llm-job:latest".into());

    let scheduling_file = std::env::var("TIGHTBEAM_SCHEDULING_FILE")
        .unwrap_or_else(|_| "/etc/sycophant/scheduling.yaml".into());
    let scheduling = shared::scheduling::SchedulingConfig::load_or_default(&scheduling_file, true)?;

    let state = Arc::new(ControllerState::new(
        conversation_factory,
        Some(kube_client.clone()),
        namespace.clone(),
        controller_addr,
        llm_job_image,
        scheduling,
    ));

    // Shared between client_watcher (writes registrations on Apply,
    // removes on Delete) and the external listener's middleware (reads
    // on every signed request).
    let client_signature_verifier = Arc::new(ClientSignatureVerifier::new(DEFAULT_WINDOW));
    let signing_key_for_watcher = Arc::new(signing_key.clone());

    {
        let (model_ready_tx, mut model_ready_rx) = tokio::sync::watch::channel(false);
        let (provider_ready_tx, mut provider_ready_rx) = tokio::sync::watch::channel(false);
        let (client_ready_tx, mut client_ready_rx) = tokio::sync::watch::channel(false);

        let model_state = state.clone();
        let model_ns = namespace.clone();
        tokio::spawn(async move {
            // Separate kube client per watcher to avoid HTTP/2 connection
            // multiplexing issues with the Job creation client.
            let client = match kube::Client::try_default().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("model watcher kube client failed: {e}");
                    return;
                }
            };
            if let Err(e) = tightbeam_controller::watcher::watch_models(
                client,
                &model_ns,
                model_state,
                model_ready_tx,
            )
            .await
            {
                tracing::error!("model watcher failed: {e}");
            }
        });

        let provider_state = state.clone();
        let provider_ns = namespace.clone();
        tokio::spawn(async move {
            let client = match kube::Client::try_default().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("provider watcher kube client failed: {e}");
                    return;
                }
            };
            if let Err(e) = tightbeam_controller::watcher::watch_providers(
                client,
                &provider_ns,
                provider_state,
                provider_ready_tx,
            )
            .await
            {
                tracing::error!("provider watcher failed: {e}");
            }
        });

        let client_watcher_ns = namespace.clone();
        let client_watcher_verifier = client_signature_verifier.clone();
        let client_watcher_signing_key = signing_key_for_watcher.clone();
        tokio::spawn(async move {
            let client = match kube::Client::try_default().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("client watcher kube client failed: {e}");
                    return;
                }
            };
            if let Err(e) = tightbeam_controller::client_watcher::watch_clients(
                client,
                &client_watcher_ns,
                client_watcher_signing_key,
                client_watcher_verifier,
                client_ready_tx,
            )
            .await
            {
                tracing::error!("client watcher failed: {e}");
            }
        });

        match tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let _ = tokio::join!(
                model_ready_rx.wait_for(|&v| v),
                provider_ready_rx.wait_for(|&v| v),
                client_ready_rx.wait_for(|&v| v),
            );
        })
        .await
        {
            Ok(_) => tracing::info!("watcher initial sync complete"),
            Err(_) => tracing::warn!("watcher sync timed out after 10s, serving anyway"),
        };
    }

    let internal_service =
        ControllerService::internal(state.clone(), verifier, signing_key.clone());
    let external_service = ControllerService::external(state.clone(), signing_key);

    let internal_addr = format!("0.0.0.0:{DEFAULT_INTERNAL_GRPC_PORT}").parse()?;
    let external_addr = format!("127.0.0.1:{DEFAULT_EXTERNAL_GRPC_PORT}").parse()?;

    let (health_reporter, internal_health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<TightbeamControllerServer<ControllerService>>()
        .await;

    let internal_reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(tightbeam_proto::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    tracing::info!(
        internal = %internal_addr,
        external = %external_addr,
        "tightbeam-controller listening on two-listener split"
    );

    let internal = Server::builder()
        .add_service(internal_reflection)
        .add_service(internal_health_service)
        .add_service(TightbeamControllerServer::new(internal_service))
        .serve(internal_addr);

    let external = Server::builder()
        .layer(SignatureLayer::new(client_signature_verifier.clone()))
        .add_service(TightbeamControllerServer::new(external_service))
        .serve(external_addr);

    tokio::try_join!(internal, external)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_internal_verifier_returns_none_when_no_kube_client() {
        assert!(build_internal_verifier(None).is_none());
    }

    #[test]
    fn internal_and_external_ports_are_distinct() {
        // Mutants love changing port literals; pin the invariant that
        // the two listeners bind different ports.
        assert_ne!(DEFAULT_INTERNAL_GRPC_PORT, DEFAULT_EXTERNAL_GRPC_PORT);
    }

    #[test]
    fn internal_port_is_9090() {
        assert_eq!(DEFAULT_INTERNAL_GRPC_PORT, 9090);
    }

    #[test]
    fn external_port_is_9091() {
        assert_eq!(DEFAULT_EXTERNAL_GRPC_PORT, 9091);
    }

    fn write_key_file(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("key");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn load_signing_key_returns_key_when_file_has_32_bytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let key_bytes = [7u8; 32];
        let path = write_key_file(tmp.path(), &key_bytes);

        let loaded = load_signing_key(&path).unwrap();

        assert_eq!(
            loaded.to_bytes(),
            key_bytes,
            "loaded key must round-trip the on-disk bytes"
        );
    }

    #[test]
    fn load_signing_key_returns_same_key_across_calls() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = write_key_file(tmp.path(), &[42u8; 32]);

        let first = load_signing_key(&path).unwrap();
        let second = load_signing_key(&path).unwrap();

        assert_eq!(
            first.to_bytes(),
            second.to_bytes(),
            "consecutive loads of the same file must return the same key"
        );
    }

    #[test]
    fn load_signing_key_errors_when_file_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("does-not-exist");

        let err = load_signing_key(&path).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        let msg = err.to_string();
        assert!(
            msg.contains("tightbeam-signing-key"),
            "error must point at the chart's Secret name, got: {msg}"
        );
    }

    #[test]
    fn load_signing_key_errors_when_file_wrong_size() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = write_key_file(tmp.path(), &[0u8; 16]);

        let err = load_signing_key(&path).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(
            msg.contains("32 bytes"),
            "error must state the expected key length, got: {msg}"
        );
        assert!(
            msg.contains("got 16"),
            "error must report the observed length, got: {msg}"
        );
    }

    #[test]
    fn parse_bool_env_returns_true_for_literal_true() {
        assert!(parse_bool_env(Some("true".into()), false));
    }

    #[test]
    fn parse_bool_env_returns_false_for_anything_other_than_literal_true() {
        assert!(!parse_bool_env(Some("True".into()), true));
        assert!(!parse_bool_env(Some("1".into()), true));
        assert!(!parse_bool_env(Some("false".into()), true));
        assert!(!parse_bool_env(Some("".into()), true));
    }

    #[test]
    fn parse_bool_env_returns_default_when_unset() {
        assert!(parse_bool_env(None, true));
        assert!(!parse_bool_env(None, false));
    }
}
