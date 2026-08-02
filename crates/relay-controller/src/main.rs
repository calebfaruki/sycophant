use clap::Parser;
use ed25519_dalek::SigningKey;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::ByteString;
use kube::api::{Api, ObjectMeta, PostParams};
use relay_controller::gateway::GatewayService;
use relay_controller::internal::InternalService;
use relay_controller::signature_layer::SignatureLayer;
use relay_controller::state::GatewayState;
use relay_proto::relay_gateway_server::RelayGatewayServer;
use relay_proto::relay_internal_server::RelayInternalServer;
use shared::auth::{K8sTokenVerifier, TokenVerifier, HARNESS_RELAY_AUDIENCE};
use shared::client_signature::ClientSignatureVerifier;
use shared::replay_cache::DEFAULT_WINDOW;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tonic::transport::Server;

/// Internal listener: K8s SA token via TokenReview. Bound `0.0.0.0` so
/// in-cluster workloads (the harness, hangar) can reach it.
const DEFAULT_INTERNAL_GRPC_PORT: u16 = 9090;
/// External listener: signed-request envelope verified by
/// `signature_layer` tower middleware. Bound `127.0.0.1` so only the
/// tsnet-bridge sidecar in the same Pod can route to it.
const DEFAULT_EXTERNAL_GRPC_PORT: u16 = 9091;

const SIGNING_KEY_SECRET_NAME: &str = "relay-signing-key";
const SIGNING_KEY_SECRET_FIELD: &str = "key";
const BOOTSTRAP_BUDGET: Duration = Duration::from_secs(60);
const BOOTSTRAP_BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const BOOTSTRAP_BACKOFF_CEILING: Duration = Duration::from_secs(30);

#[derive(Parser)]
#[command(
    name = "relay-controller",
    about = "Sycophant internet-facing gateway controller"
)]
struct Cli {}

/// Get-or-create the `relay-signing-key` Secret in `namespace`. On
/// first install the Secret is absent; we mint 32 random bytes (Ed25519
/// seed), create the Secret, and return the key. On restart the Secret
/// exists; we read and return. Race-safe via 409 retry.
///
/// RBAC cache may lag the RoleBinding by a few seconds on fresh install;
/// we retry 403 with exponential backoff up to `BOOTSTRAP_BUDGET`.
async fn bootstrap_signing_key(
    client: &kube::Client,
    namespace: &str,
) -> Result<SigningKey, Box<dyn std::error::Error>> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let deadline = compute_bootstrap_deadline(Instant::now());
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
            Err(kube::Error::Api(e)) => match classify_get_error(e.code) {
                BootstrapStep::Mint => {
                    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
                    let secret = build_signing_key_secret(namespace, &sk);
                    match api.create(&PostParams::default(), &secret).await {
                        Ok(_) => {
                            tracing::info!(
                                secret = SIGNING_KEY_SECRET_NAME,
                                namespace,
                                "minted and created signing key Secret"
                            );
                            return Ok(sk);
                        }
                        Err(kube::Error::Api(e)) => match classify_create_error(e.code) {
                            BootstrapStep::RereadAfterRace => continue,
                            BootstrapStep::BackoffRbac => {
                                wait_for_rbac_propagation(
                                    &mut backoff,
                                    deadline,
                                    "create",
                                    &e.message,
                                )
                                .await?;
                            }
                            _ => return Err(kube::Error::Api(e).into()),
                        },
                        Err(e) => return Err(e.into()),
                    }
                }
                BootstrapStep::BackoffRbac => {
                    wait_for_rbac_propagation(&mut backoff, deadline, "get", &e.message).await?;
                }
                _ => return Err(kube::Error::Api(e).into()),
            },
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
            "relay-signing-key bootstrap: {op} returned 403 beyond deadline ({}s): {api_msg}",
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

/// One step of the signing-key bootstrap loop, chosen from a kube API
/// status code. Extracted so the code-to-action mapping is covered by
/// pure unit tests (mirrors `enrollment_store::map_kube_get`).
#[derive(Debug, PartialEq, Eq)]
enum BootstrapStep {
    Mint,
    RereadAfterRace,
    BackoffRbac,
    Fail,
}

/// Map a `get` failure code: 404 → mint, 403 → back off for RBAC
/// propagation, anything else → fail.
fn classify_get_error(code: u16) -> BootstrapStep {
    match code {
        404 => BootstrapStep::Mint,
        403 => BootstrapStep::BackoffRbac,
        _ => BootstrapStep::Fail,
    }
}

/// Map a `create` failure code: 409 → another writer won the race, reread;
/// 403 → back off for RBAC propagation; anything else → fail.
fn classify_create_error(code: u16) -> BootstrapStep {
    match code {
        409 => BootstrapStep::RereadAfterRace,
        403 => BootstrapStep::BackoffRbac,
        _ => BootstrapStep::Fail,
    }
}

/// Deadline after which the bootstrap loop stops retrying 403s.
fn compute_bootstrap_deadline(now: Instant) -> Instant {
    now + BOOTSTRAP_BUDGET
}

/// Build the `relay-signing-key` Secret carrying the Ed25519 seed.
fn build_signing_key_secret(namespace: &str, signing_key: &SigningKey) -> Secret {
    let mut data = BTreeMap::new();
    data.insert(
        SIGNING_KEY_SECRET_FIELD.into(),
        ByteString(signing_key.to_bytes().to_vec()),
    );
    Secret {
        metadata: ObjectMeta {
            name: Some(SIGNING_KEY_SECRET_NAME.into()),
            namespace: Some(namespace.into()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    }
}

/// Build the internal-listener token verifier. Pins
/// `harness.relay` — the harness is the primary live caller
/// of the internal surface (`Subscribe` + the server-request methods).
///
/// NOTE: `DeliverOutbound` is dialed by hangar (audience
/// `hangar.relay`) in a later refactor stage; when that path goes
/// live the internal listener needs a per-method verifier pair (mirroring
/// hangar's audience_layer) so a harness token cannot reach
/// `DeliverOutbound` and a hangar token cannot reach `Subscribe`. Single
/// audience here keeps the single-audience-token invariant intact for the
/// surface that is live today.
fn build_internal_verifier(kube_client: Option<&kube::Client>) -> Option<Arc<dyn TokenVerifier>> {
    kube_client.map(|c| {
        Arc::new(K8sTokenVerifier::new(c.clone(), HARNESS_RELAY_AUDIENCE)) as Arc<dyn TokenVerifier>
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().json().with_target(false).init();

    // Pin the rustls 0.23 CryptoProvider; refuses to auto-pick when
    // multiple are compiled in.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let _cli = Cli::parse();

    let kube_client = shared::try_init_kube_client().await?;

    let namespace = std::env::var("RELAY_NAMESPACE").unwrap_or_else(|_| "default".into());

    let signing_key = bootstrap_signing_key(&kube_client, &namespace).await?;

    // Shared between enrollment_watcher (writes registrations on Apply,
    // removes on Delete) and the external listener's middleware (reads on
    // every signed request).
    let enrollment_verifier = Arc::new(ClientSignatureVerifier::new(DEFAULT_WINDOW));
    let signing_key_for_watcher = Arc::new(signing_key.clone());

    let state = Arc::new(GatewayState::new(
        enrollment_verifier.clone(),
        signing_key,
        Some(kube_client.clone()),
        namespace.clone(),
    ));

    // Enrollment CR watcher: mints codes for fresh Enrollments, installs
    // registered public keys into the verifier cache.
    {
        let (enrollment_ready_tx, mut enrollment_ready_rx) = tokio::sync::watch::channel(false);
        let watcher_ns = namespace.clone();
        let watcher_verifier = enrollment_verifier.clone();
        let watcher_signing_key = signing_key_for_watcher.clone();
        let watcher_client = kube_client.clone();
        shared::watcher_retry::spawn_watcher_task("enrollments", move || {
            let ns = watcher_ns.clone();
            let client = watcher_client.clone();
            let signing_key = watcher_signing_key.clone();
            let verifier = watcher_verifier.clone();
            let tx = enrollment_ready_tx.clone();
            async move {
                relay_controller::enrollment_watcher::watch_enrollments(
                    client,
                    &ns,
                    signing_key,
                    verifier,
                    tx,
                )
                .await
            }
        });

        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            enrollment_ready_rx.wait_for(|&v| v),
        )
        .await
        {
            Ok(_) => tracing::info!("enrollment watcher initial sync complete"),
            Err(_) => tracing::warn!("enrollment watcher sync timed out after 10s, serving anyway"),
        };
    }

    let internal_verifier = build_internal_verifier(Some(&kube_client));
    let internal_service = InternalService::new(state.clone(), internal_verifier);
    let external_service = GatewayService::new(state.clone());

    let internal_addr = format!("0.0.0.0:{DEFAULT_INTERNAL_GRPC_PORT}").parse()?;
    let external_addr = format!("127.0.0.1:{DEFAULT_EXTERNAL_GRPC_PORT}").parse()?;

    let (health_reporter, internal_health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<RelayInternalServer<InternalService>>()
        .await;

    let internal_reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(relay_proto::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    tracing::info!(
        internal = %internal_addr,
        external = %external_addr,
        "relay-controller listening on two-listener split"
    );

    let internal = Server::builder()
        .add_service(internal_reflection)
        .add_service(internal_health_service)
        .add_service(RelayInternalServer::new(internal_service))
        .serve(internal_addr);

    let external = Server::builder()
        .layer(SignatureLayer::new(enrollment_verifier.clone()))
        .add_service(RelayGatewayServer::new(external_service))
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
    fn signing_key_secret_name_is_relay_scoped() {
        assert_eq!(SIGNING_KEY_SECRET_NAME, "relay-signing-key");
    }

    #[test]
    fn classify_get_404_mints() {
        assert_eq!(classify_get_error(404), BootstrapStep::Mint);
    }

    #[test]
    fn classify_get_403_backs_off() {
        assert_eq!(classify_get_error(403), BootstrapStep::BackoffRbac);
    }

    #[test]
    fn classify_get_other_codes_fail() {
        assert_eq!(classify_get_error(500), BootstrapStep::Fail);
        assert_eq!(classify_get_error(409), BootstrapStep::Fail);
        assert_eq!(classify_get_error(200), BootstrapStep::Fail);
    }

    #[test]
    fn classify_create_409_rereads() {
        assert_eq!(classify_create_error(409), BootstrapStep::RereadAfterRace);
    }

    #[test]
    fn classify_create_403_backs_off() {
        assert_eq!(classify_create_error(403), BootstrapStep::BackoffRbac);
    }

    #[test]
    fn classify_create_other_codes_fail() {
        assert_eq!(classify_create_error(500), BootstrapStep::Fail);
        assert_eq!(classify_create_error(404), BootstrapStep::Fail);
        assert_eq!(classify_create_error(200), BootstrapStep::Fail);
    }

    #[test]
    fn signing_key_secret_has_name() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let secret = build_signing_key_secret("any-ns", &sk);
        assert_eq!(secret.metadata.name.as_deref(), Some("relay-signing-key"));
    }

    #[test]
    fn signing_key_secret_has_namespace() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let secret = build_signing_key_secret("my-ns", &sk);
        assert_eq!(secret.metadata.namespace.as_deref(), Some("my-ns"));
    }

    #[test]
    fn signing_key_secret_data_round_trips_to_same_key() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let secret = build_signing_key_secret("any-ns", &sk);
        let bytes = extract_key_bytes(&secret).unwrap();
        assert_eq!(bytes, sk.to_bytes());
    }

    #[test]
    fn bootstrap_deadline_is_budget_into_the_future() {
        let now = Instant::now();
        assert!(compute_bootstrap_deadline(now) > now);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_rbac_propagation_errors_once_deadline_passed() {
        let mut backoff = BOOTSTRAP_BACKOFF_INITIAL;
        let deadline = Instant::now() - Duration::from_secs(1);
        let result = wait_for_rbac_propagation(&mut backoff, deadline, "get", "denied").await;
        assert!(result.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_rbac_propagation_retries_while_budget_remains() {
        let mut backoff = BOOTSTRAP_BACKOFF_INITIAL;
        let deadline = Instant::now() + BOOTSTRAP_BUDGET;
        let result = wait_for_rbac_propagation(&mut backoff, deadline, "get", "denied").await;
        assert!(result.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_rbac_propagation_doubles_backoff() {
        let mut backoff = BOOTSTRAP_BACKOFF_INITIAL;
        let deadline = Instant::now() + BOOTSTRAP_BUDGET;
        wait_for_rbac_propagation(&mut backoff, deadline, "get", "denied")
            .await
            .unwrap();
        assert_eq!(backoff, BOOTSTRAP_BACKOFF_INITIAL * 2);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_rbac_propagation_caps_backoff_at_ceiling() {
        let mut backoff = BOOTSTRAP_BACKOFF_CEILING;
        let deadline = Instant::now() + BOOTSTRAP_BUDGET;
        wait_for_rbac_propagation(&mut backoff, deadline, "get", "denied")
            .await
            .unwrap();
        assert_eq!(backoff, BOOTSTRAP_BACKOFF_CEILING);
    }
}
