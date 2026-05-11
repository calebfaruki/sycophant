use clap::{Parser, Subcommand};
use ed25519_dalek::{SigningKey, VerifyingKey};
use shared::auth::{CompositeVerifier, JwtVerifier, K8sTokenVerifier};
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
const SIGNING_KEY_FILENAME: &str = ".signing_key";
const DEFAULT_ENROLLMENT_TTL_SECS: i64 = 3600; // 1 hour

#[derive(Parser)]
#[command(
    name = "tightbeam-controller",
    about = "Sycophant tightbeam controller"
)]
struct Cli {
    /// Log directory (also where the JWT signing key persists). Default
    /// /var/log/tightbeam. When invoked without a subcommand this is the
    /// implicit positional arg for the default `serve` mode, preserving
    /// backwards compatibility with the chart's `args:` (none).
    #[arg(global = true, value_name = "LOG_DIR")]
    log_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the gRPC controller server (the default behavior; this subcommand
    /// is implicit when no subcommand is given so the chart's `args:` works
    /// without modification).
    Serve,
    /// Mint a one-time enrollment code for a specific device. Operator runs
    /// this via `kubectl exec deploy/tightbeam-controller -- ...`. Prints the
    /// signed code to stdout for the operator to deliver to the user.
    MintEnrollment {
        /// Workspace the device will be scoped to (must match a sycophant
        /// workspace name).
        workspace: String,
        /// Operator-assigned device name (e.g. "calebs-iphone").
        device_name: String,
        /// Code lifetime in seconds. Default 3600 (1 hour).
        #[arg(long, default_value_t = DEFAULT_ENROLLMENT_TTL_SECS)]
        ttl_secs: i64,
    },
}

/// Load the device-token signing key from the log PVC, generating a fresh
/// Ed25519 keypair on first run.
///
/// The key file lives at `<log_dir>/.signing_key` (32 raw private-key bytes,
/// chmod 0600). The log PVC is RW-mounted into the controller pod even
/// though the root FS is `readOnlyRootFilesystem: true` — the key file
/// MUST live inside that PVC mount, not anywhere else on the FS.
///
/// Auto-generation makes deployment hands-off (no Secret to pre-create) and
/// the persistence makes restart-safe. Operator can rotate by deleting the
/// file and rolling the controller; all existing JWTs become unverifiable
/// (effectively a nuclear revoke).
fn load_or_generate_signing_key(log_dir: &Path) -> std::io::Result<SigningKey> {
    use std::os::unix::fs::PermissionsExt;
    let key_path = log_dir.join(SIGNING_KEY_FILENAME);
    if key_path.exists() {
        let bytes = std::fs::read(&key_path)?;
        let bytes_array: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "signing key file is not 32 bytes",
            )
        })?;
        tracing::info!(path = %key_path.display(), "loaded existing signing key");
        Ok(SigningKey::from_bytes(&bytes_array))
    } else {
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        std::fs::write(&key_path, signing_key.to_bytes())?;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&key_path, perms)?;
        tracing::info!(path = %key_path.display(), "generated new signing key");
        Ok(signing_key)
    }
}

/// Build the auth verifier from whichever credentials are available.
///
/// Both verifiers run concurrently for every request via `CompositeVerifier`
/// (JWT first — cheap local Ed25519 check; K8s `TokenReview` fallback for
/// in-cluster SA tokens). This matches the actual call shape: external
/// clients (mobile via tsnet) present device JWTs minted by `EnrollDevice`;
/// in-cluster pods (transponder/llm-job/channel-job) present projected
/// ServiceAccount tokens. Returns `None` only when nothing's available.
fn build_verifier(
    kube_client: Option<&kube::Client>,
    verifying_key: Option<VerifyingKey>,
) -> Option<Arc<dyn shared::auth::TokenVerifier>> {
    let mut verifiers: Vec<Arc<dyn shared::auth::TokenVerifier>> = Vec::new();
    if let Some(vk) = verifying_key {
        verifiers.push(Arc::new(JwtVerifier::new(vk)));
    }
    if let Some(c) = kube_client {
        verifiers.push(Arc::new(K8sTokenVerifier::new(c.clone())));
    }
    if verifiers.is_empty() {
        None
    } else {
        Some(Arc::new(CompositeVerifier::new(verifiers)))
    }
}

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

    let cli = Cli::parse();
    let log_dir = cli.log_dir.unwrap_or_else(|| DEFAULT_LOG_DIR.into());

    // Ensure the log dir exists for both subcommands — `serve` uses it for
    // the signing key + workspace logs; `mint-enrollment` reads the signing
    // key from it.
    std::fs::create_dir_all(&log_dir)
        .map_err(|e| format!("failed to create log_dir {}: {e}", log_dir.display()))?;

    if let Some(Command::MintEnrollment {
        workspace,
        device_name,
        ttl_secs,
    }) = cli.command
    {
        let signing_key = load_or_generate_signing_key(&log_dir)?;
        let code_id = uuid::Uuid::new_v4().to_string();
        let code = shared::auth::sign_enrollment_code(
            &signing_key,
            &workspace,
            &device_name,
            &code_id,
            ttl_secs,
        );
        // Print just the code to stdout. Operator-facing UX: copy and send.
        // Diagnostic info (workspace, device, expiry) goes to stderr so it
        // doesn't pollute the code.
        eprintln!(
            "minted enrollment code (workspace={workspace}, device={device_name}, ttl_secs={ttl_secs})"
        );
        println!("{code}");
        return Ok(());
    }

    // Default subcommand: serve. (`Command::Serve` and `None` both fall
    // through to the same path.)
    let workspace_convs = scan_workspace_convs(&log_dir);
    if workspace_convs.is_empty() {
        tracing::info!("no existing workspace logs found");
    } else {
        tracing::info!("loaded {} workspace(s) from disk", workspace_convs.len());
    }

    let kube_client = shared::try_init_kube_client().await?;

    // Signing-key load/generate runs BEFORE `build_verifier` so the verifier
    // sees the key on first boot as well as on subsequent boots. The key file
    // lives inside the existing log PVC mount (RW even though root FS is RO).
    let signing_key = load_or_generate_signing_key(&log_dir)?;
    let verifying_key = signing_key.verifying_key();

    let verifier = build_verifier(kube_client.as_ref(), Some(verifying_key));

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
        let (model_ready_tx, mut model_ready_rx) = tokio::sync::watch::channel(false);
        let (provider_ready_tx, mut provider_ready_rx) = tokio::sync::watch::channel(false);

        let model_state = state.clone();
        let model_ns = namespace.clone();
        tokio::spawn(async move {
            // Separate kube client for the watcher to avoid HTTP/2
            // connection multiplexing issues with the Job creation client.
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
        let provider_ns = namespace;
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

        match tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let _ = tokio::join!(
                model_ready_rx.wait_for(|&v| v),
                provider_ready_rx.wait_for(|&v| v),
            );
        })
        .await
        {
            Ok(_) => tracing::info!("watcher initial sync complete"),
            Err(_) => tracing::warn!("watcher sync timed out after 10s, serving anyway"),
        };
    }

    let service = ControllerService::new(state, verifier).with_signing_key(signing_key);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_verifying_key() -> VerifyingKey {
        let mut csprng = rand::rngs::OsRng;
        SigningKey::generate(&mut csprng).verifying_key()
    }

    #[test]
    fn build_verifier_returns_some_when_only_signing_key_present() {
        let vk = make_verifying_key();
        assert!(build_verifier(None, Some(vk)).is_some());
    }

    #[test]
    fn build_verifier_returns_none_when_no_kube_and_no_signing_key() {
        // Neither path configured → no verifier. Controller will refuse
        // to authenticate any request (FailedPrecondition at call sites).
        assert!(build_verifier(None, None).is_none());
    }

    #[test]
    fn load_or_generate_signing_key_creates_file_on_first_call() {
        let tmp = tempfile::TempDir::new().unwrap();
        let key_path = tmp.path().join(SIGNING_KEY_FILENAME);
        assert!(!key_path.exists(), "precondition: key file must not exist");

        let _ = load_or_generate_signing_key(tmp.path()).unwrap();

        assert!(key_path.exists(), "key file must be created");
        let bytes = std::fs::read(&key_path).unwrap();
        assert_eq!(bytes.len(), 32, "key file must be 32 raw bytes");
    }

    #[test]
    fn load_or_generate_signing_key_reuses_existing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let first = load_or_generate_signing_key(tmp.path()).unwrap();
        let second = load_or_generate_signing_key(tmp.path()).unwrap();
        assert_eq!(
            first.to_bytes(),
            second.to_bytes(),
            "second call must return the same key bytes"
        );
    }

    #[test]
    fn load_or_generate_signing_key_sets_owner_only_perms() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let _ = load_or_generate_signing_key(tmp.path()).unwrap();
        let key_path = tmp.path().join(SIGNING_KEY_FILENAME);
        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "key file must be chmod 0600 (owner read/write only)"
        );
    }
}
