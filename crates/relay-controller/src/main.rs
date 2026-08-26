use clap::Parser;
use relay_controller::gateway::GatewayService;
use relay_controller::internal::InternalService;
use relay_controller::signature_layer::SignatureLayer;
use relay_controller::state::GatewayState;
use relay_proto::relay_gateway_server::RelayGatewayServer;
use relay_proto::relay_internal_server::RelayInternalServer;
use shared::auth::{K8sTokenVerifier, TokenVerifier, HARNESS_RELAY_AUDIENCE};
use shared::client_signature::ClientSignatureVerifier;
use shared::replay_cache::DEFAULT_WINDOW;
use std::sync::Arc;
use tonic::transport::Server;

/// Internal listener: K8s SA token via TokenReview. Bound `0.0.0.0` so
/// in-cluster workloads (the harness) can reach it.
const DEFAULT_INTERNAL_GRPC_PORT: u16 = 9090;
/// App listener: signed-request envelope verified by the
/// `signature_layer` tower middleware. Bound on the pod network and
/// admitted only from the app adapter pod by the relay's ingress
/// CiliumNetworkPolicy.
const DEFAULT_APP_GRPC_PORT: u16 = 9091;
/// Adapter listener: the same signed-request envelope, on its own
/// socket, admitted only from `adapter-class: principal` pods.
const DEFAULT_ADAPTER_GRPC_PORT: u16 = 9092;

/// One `tonic` server future, boxed so the listener table can hold the
/// internal and client-signed servers side by side.
type ServerFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), tonic::transport::Error>> + Send>,
>;

/// The credential a listener demands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerAuth {
    /// Kubernetes SA token, verified by TokenReview. The harness link.
    TokenReview,
    /// Client-signed envelope, verified by `SignatureLayer`.
    ClientSignature,
}

/// Every socket the relay serves, paired with the credential it demands.
/// `main` builds its servers by walking this table.
fn listeners() -> Vec<(std::net::SocketAddr, ListenerAuth)> {
    [
        (DEFAULT_INTERNAL_GRPC_PORT, ListenerAuth::TokenReview),
        (DEFAULT_APP_GRPC_PORT, ListenerAuth::ClientSignature),
        (DEFAULT_ADAPTER_GRPC_PORT, ListenerAuth::ClientSignature),
    ]
    .into_iter()
    .map(|(port, auth)| {
        (
            std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port)),
            auth,
        )
    })
    .collect()
}

#[derive(Parser)]
#[command(
    name = "relay-controller",
    about = "Sycophant internet-facing gateway controller"
)]
struct Cli {}

/// Build the internal-listener token verifier. Pins
/// `harness.relay` — the harness is the sole live caller of the
/// internal surface (`Subscribe`, the server-request methods, and
/// `DeliverOutbound`).
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

    // Shared between the signature middleware (reads on every signed
    // request), the startup rebuild, and each redemption.
    let client_verifier = Arc::new(ClientSignatureVerifier::new(DEFAULT_WINDOW));

    // The per-workspace credential-grant menu, from the chart-mounted
    // bindings file. A configured-but-unreadable file is a startup failure;
    // an unconfigured one leaves the menu empty.
    let credentials = match std::env::var("TOOLSET_BINDINGS_FILE") {
        Ok(path) => relay_controller::credentials::CredentialMenu::load(&path)?,
        Err(_) => {
            tracing::info!("TOOLSET_BINDINGS_FILE unset; credential menu is empty");
            relay_controller::credentials::CredentialMenu::default()
        }
    };

    let state = Arc::new(
        GatewayState::new(
            client_verifier.clone(),
            Some(kube_client.clone()),
            namespace.clone(),
        )
        .with_credentials(credentials),
    );

    // Grants watch: the live authorization table. Hot reload is the whole
    // revocation promise — a removed row must cut access within seconds,
    // without a pod restart.
    let grants = state.grants();
    {
        let (grants_ready_tx, mut grants_ready_rx) = tokio::sync::watch::channel(false);
        let watcher_ns = namespace.clone();
        let watcher_client = kube_client.clone();
        let watcher_grants = grants.clone();
        shared::watcher_retry::spawn_watcher_task("grants", move || {
            let ns = watcher_ns.clone();
            let client = watcher_client.clone();
            let table = watcher_grants.clone();
            let tx = grants_ready_tx.clone();
            async move { relay_controller::grants_watcher::watch_grants(client, &ns, table, tx).await }
        });

        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            grants_ready_rx.wait_for(|&v| v),
        )
        .await
        {
            Ok(_) => tracing::info!("grants watcher initial sync complete"),
            // Serving past this point is useless and silent: an unsynced
            // grants table is empty, so it authorizes nobody, and the key
            // load below installs nothing against it. Crash instead, and
            // let the kubelet's restart backoff retry.
            Err(_) => return Err("grants watcher initial sync did not complete within 10s".into()),
        };
    }

    // Rebuild the verifier from the relay-owned registered-key Secret, so a
    // redeemed device survives a pod roll. Narrowed by the live grants
    // table: a key whose row is gone is never reinstalled.
    {
        let table = grants.read().await;
        match relay_controller::registered_keys::load_into_verifier(
            &kube_client,
            &namespace,
            &table,
            &client_verifier,
        )
        .await
        {
            Ok(n) => tracing::info!(installed = n, "registered keys loaded"),
            // Same reasoning: a failed load leaves every redeemed device
            // unverifiable with no path back short of a restart, so take
            // the restart now rather than serving a relay nobody can reach.
            Err(e) => return Err(format!("loading registered keys: {e}").into()),
        }
    }

    let table = listeners();
    tracing::info!(
        listeners = ?table.iter().map(|(a, auth)| format!("{a} {auth:?}")).collect::<Vec<_>>(),
        "relay-controller listening"
    );

    let mut servers: Vec<ServerFuture> = Vec::with_capacity(table.len());

    for (addr, auth) in table {
        match auth {
            ListenerAuth::TokenReview => {
                let internal_verifier = build_internal_verifier(Some(&kube_client));
                let internal_service = InternalService::new(state.clone(), internal_verifier);
                let (health_reporter, health_service) = tonic_health::server::health_reporter();
                health_reporter
                    .set_serving::<RelayInternalServer<InternalService>>()
                    .await;
                let reflection = tonic_reflection::server::Builder::configure()
                    .register_encoded_file_descriptor_set(relay_proto::FILE_DESCRIPTOR_SET)
                    .build_v1()?;
                servers.push(Box::pin(
                    Server::builder()
                        .add_service(reflection)
                        .add_service(health_service)
                        .add_service(RelayInternalServer::new(internal_service))
                        .serve(addr),
                ));
            }
            ListenerAuth::ClientSignature => {
                servers.push(Box::pin(
                    Server::builder()
                        .layer(SignatureLayer::new(client_verifier.clone()))
                        .add_service(RelayGatewayServer::new(GatewayService::new(state.clone())))
                        .serve(addr),
                ));
            }
        }
    }

    futures::future::try_join_all(servers).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_internal_verifier_returns_none_when_no_kube_client() {
        assert!(build_internal_verifier(None).is_none());
    }

    // `main` builds its servers by walking `listeners()`, wrapping every
    // `ClientSignature` entry in `SignatureLayer` and no `TokenReview` entry.
    // The tests below constrain that table rather than describing it.

    #[test]
    fn adapter_port_is_9092() {
        assert_eq!(DEFAULT_ADAPTER_GRPC_PORT, 9092);
    }

    // Drop the adapter entry from the table and the count is two; give it 9091
    // or 9090 and the distinctness assertion reds. The relay would then have no
    // adapter port at all while every other test in the suite stayed green.
    #[test]
    fn three_listeners_are_served_on_three_distinct_sockets() {
        let table = listeners();
        assert_eq!(table.len(), 3, "internal, app, and adapter");
        let mut ports: Vec<u16> = table.iter().map(|(a, _)| a.port()).collect();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(ports.len(), 3, "listener ports must be distinct");
        assert!(ports.contains(&DEFAULT_INTERNAL_GRPC_PORT));
        assert!(ports.contains(&DEFAULT_APP_GRPC_PORT));
        assert!(ports.contains(&DEFAULT_ADAPTER_GRPC_PORT));
    }

    // The app port binds off loopback and the adapter port is in-cluster from
    // birth. Both are admitted by the relay's ingress CNP, not by the pod
    // boundary.
    //
    // Leave `format!("127.0.0.1:{DEFAULT_APP_GRPC_PORT}")` in place and the app
    // adapter, a separate Deployment, can never reach the relay. Nothing else in
    // the Rust suite observes the bind address.
    #[test]
    fn app_and_adapter_listeners_bind_in_cluster_addresses() {
        for (addr, _) in listeners() {
            assert!(
                !addr.ip().is_loopback(),
                "{addr} binds loopback; no listener is same-pod any more"
            );
        }
    }

    // The adapter socket carries the same client-signature verification as the
    // app port. List it as `TokenReview`, or omit it from the signature-verified
    // set, and the relay serves an unverified gRPC port to every pod the ingress
    // CNP admits. A reachability test on the happy path cannot see this; the port
    // answers either way.
    #[test]
    fn client_signature_covers_the_app_and_adapter_listeners_only() {
        let table = listeners();
        let signed: Vec<u16> = table
            .iter()
            .filter(|(_, auth)| *auth == ListenerAuth::ClientSignature)
            .map(|(a, _)| a.port())
            .collect();
        assert_eq!(signed.len(), 2, "app and adapter, and nothing else");
        assert!(signed.contains(&DEFAULT_APP_GRPC_PORT));
        assert!(signed.contains(&DEFAULT_ADAPTER_GRPC_PORT));

        let token_reviewed: Vec<u16> = table
            .iter()
            .filter(|(_, auth)| *auth == ListenerAuth::TokenReview)
            .map(|(a, _)| a.port())
            .collect();
        assert_eq!(
            token_reviewed,
            vec![DEFAULT_INTERNAL_GRPC_PORT],
            "the harness link is the only TokenReview socket"
        );
    }

    #[test]
    fn internal_port_is_9090() {
        assert_eq!(DEFAULT_INTERNAL_GRPC_PORT, 9090);
    }

    #[test]
    fn app_port_is_9091() {
        assert_eq!(DEFAULT_APP_GRPC_PORT, 9091);
    }
}
