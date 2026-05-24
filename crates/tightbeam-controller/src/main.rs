use clap::Parser;
use ed25519_dalek::SigningKey;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::ByteString;
use kube::api::{Api, ObjectMeta, PostParams};
use shared::auth::K8sTokenVerifier;
use shared::client_signature::ClientSignatureVerifier;
use shared::replay_cache::DEFAULT_WINDOW;
use shared::storage::S3Spec;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
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

const SIGNING_KEY_SECRET_NAME: &str = "tightbeam-signing-key";
const SIGNING_KEY_SECRET_FIELD: &str = "key";
const BOOTSTRAP_BUDGET: Duration = Duration::from_secs(60);
const BOOTSTRAP_BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const BOOTSTRAP_BACKOFF_CEILING: Duration = Duration::from_secs(30);

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

/// Get-or-create the `tightbeam-signing-key` Secret in `namespace`. On first
/// install the Secret is absent; we mint 32 random bytes (Ed25519 seed),
/// create the Secret, and return the key. On restart the Secret exists; we
/// read and return. Race-safe via 409 retry.
///
/// RBAC cache may lag the RoleBinding by a few seconds on fresh install;
/// we retry 403 with exponential backoff up to `BOOTSTRAP_BUDGET`.
async fn bootstrap_signing_key(
    client: &kube::Client,
    namespace: &str,
) -> Result<SigningKey, Box<dyn std::error::Error>> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let deadline = Instant::now() + BOOTSTRAP_BUDGET;
    let mut backoff = BOOTSTRAP_BACKOFF_INITIAL;

    loop {
        match api.get(SIGNING_KEY_SECRET_NAME).await {
            Ok(secret) => {
                let bytes = extract_key_bytes(&secret)?;
                tracing::info!(
                    secret = SIGNING_KEY_SECRET_NAME,
                    namespace,
                    "loaded signing key from existing Secret"
                );
                return Ok(SigningKey::from_bytes(&bytes));
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {
                let sk = SigningKey::generate(&mut rand::rngs::OsRng);
                let secret = Secret {
                    metadata: ObjectMeta {
                        name: Some(SIGNING_KEY_SECRET_NAME.into()),
                        namespace: Some(namespace.into()),
                        ..Default::default()
                    },
                    data: Some({
                        let mut m = BTreeMap::new();
                        m.insert(
                            SIGNING_KEY_SECRET_FIELD.into(),
                            ByteString(sk.to_bytes().to_vec()),
                        );
                        m
                    }),
                    ..Default::default()
                };
                match api.create(&PostParams::default(), &secret).await {
                    Ok(_) => {
                        tracing::info!(
                            secret = SIGNING_KEY_SECRET_NAME,
                            namespace,
                            "minted and created signing key Secret"
                        );
                        return Ok(sk);
                    }
                    Err(kube::Error::Api(e)) if e.code == 409 => continue,
                    Err(kube::Error::Api(e)) if e.code == 403 => {
                        wait_for_rbac_propagation(&mut backoff, deadline, "create", &e.message)
                            .await?;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Err(kube::Error::Api(e)) if e.code == 403 => {
                wait_for_rbac_propagation(&mut backoff, deadline, "get", &e.message).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Extract the 32-byte signing key from a Secret's `key` data field.
fn extract_key_bytes(secret: &Secret) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let data = secret
        .data
        .as_ref()
        .ok_or_else(|| format!("Secret {SIGNING_KEY_SECRET_NAME} has no data field"))?;
    let entry = data.get(SIGNING_KEY_SECRET_FIELD).ok_or_else(|| {
        format!("Secret {SIGNING_KEY_SECRET_NAME} missing data.{SIGNING_KEY_SECRET_FIELD} field")
    })?;
    let bytes: [u8; 32] = entry.0.as_slice().try_into().map_err(|_| {
        format!(
            "Secret {SIGNING_KEY_SECRET_NAME} data.{SIGNING_KEY_SECRET_FIELD} must be 32 bytes, got {}",
            entry.0.len()
        )
    })?;
    Ok(bytes)
}

/// Sleep `*backoff`, then double it (capped). Returns Err if the deadline is exceeded.
async fn wait_for_rbac_propagation(
    backoff: &mut Duration,
    deadline: Instant,
    op: &str,
    api_msg: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if Instant::now() >= deadline {
        return Err(format!(
            "tightbeam-signing-key bootstrap: {op} returned 403 beyond deadline ({}s): {api_msg}",
            BOOTSTRAP_BUDGET.as_secs(),
        )
        .into());
    }
    tracing::warn!(
        op,
        backoff_ms = backoff.as_millis(),
        "403 from kube-apiserver (RBAC propagation?); retrying"
    );
    tokio::time::sleep(*backoff).await;
    *backoff = (*backoff * 2).min(BOOTSTRAP_BACKOFF_CEILING);
    Ok(())
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

    let conversation_factory = build_conversation_factory(log_dir.clone()).await?;

    let kube_client = shared::try_init_kube_client().await?;

    let namespace = std::env::var("TIGHTBEAM_NAMESPACE").unwrap_or_else(|_| "default".into());

    let signing_key = bootstrap_signing_key(&kube_client, &namespace).await?;

    let verifier = build_internal_verifier(Some(&kube_client));
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
        let model_client = kube_client.clone();
        shared::watcher_retry::spawn_watcher_task("models", move || {
            let ns = model_ns.clone();
            let client = model_client.clone();
            let state = model_state.clone();
            let tx = model_ready_tx.clone();
            async move { tightbeam_controller::watcher::watch_models(client, &ns, state, tx).await }
        });

        let provider_state = state.clone();
        let provider_ns = namespace.clone();
        let provider_client = kube_client.clone();
        shared::watcher_retry::spawn_watcher_task("providers", move || {
            let ns = provider_ns.clone();
            let client = provider_client.clone();
            let state = provider_state.clone();
            let tx = provider_ready_tx.clone();
            async move { tightbeam_controller::watcher::watch_providers(client, &ns, state, tx).await }
        });

        let client_watcher_ns = namespace.clone();
        let client_watcher_verifier = client_signature_verifier.clone();
        let client_watcher_signing_key = signing_key_for_watcher.clone();
        let client_watcher_client = kube_client.clone();
        shared::watcher_retry::spawn_watcher_task("clients", move || {
            let ns = client_watcher_ns.clone();
            let client = client_watcher_client.clone();
            let signing_key = client_watcher_signing_key.clone();
            let verifier = client_watcher_verifier.clone();
            let tx = client_ready_tx.clone();
            async move {
                tightbeam_controller::client_watcher::watch_clients(
                    client,
                    &ns,
                    signing_key,
                    verifier,
                    tx,
                )
                .await
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

    fn secret_with_data(data: Option<BTreeMap<String, ByteString>>) -> Secret {
        Secret {
            data,
            ..Default::default()
        }
    }

    #[test]
    fn extract_key_bytes_returns_array_when_field_is_32_bytes() {
        let mut data = BTreeMap::new();
        data.insert("key".into(), ByteString(vec![7u8; 32]));

        let bytes = extract_key_bytes(&secret_with_data(Some(data))).unwrap();

        assert_eq!(bytes, [7u8; 32]);
    }

    #[test]
    fn extract_key_bytes_errors_when_data_missing() {
        let err = extract_key_bytes(&secret_with_data(None)).unwrap_err();
        assert!(err.to_string().contains("no data field"));
    }

    #[test]
    fn extract_key_bytes_errors_when_field_missing() {
        let data = BTreeMap::new();
        let err = extract_key_bytes(&secret_with_data(Some(data))).unwrap_err();
        assert!(err.to_string().contains("missing data.key field"));
    }

    #[test]
    fn extract_key_bytes_errors_when_field_wrong_size() {
        let mut data = BTreeMap::new();
        data.insert("key".into(), ByteString(vec![0u8; 16]));
        let err = extract_key_bytes(&secret_with_data(Some(data))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("32 bytes"));
        assert!(msg.contains("got 16"));
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
