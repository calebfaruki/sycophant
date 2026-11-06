use clap::Parser;
use relay_controller::gateway::GatewayService;
use relay_controller::internal::InternalService;
use relay_controller::listener::{class_layer, ListenerClass};
use relay_controller::state::GatewayState;
use relay_proto::relay_gateway_server::RelayGatewayServer;
use relay_proto::relay_internal_server::RelayInternalServer;
use shared::auth::{K8sTokenVerifier, TokenVerifier, HARNESS_RELAY_AUDIENCE};
use std::sync::Arc;
use tonic::transport::Server;

/// Internal listener: K8s SA token via TokenReview. Bound `0.0.0.0` so
/// in-cluster workloads (the harness) can reach it.
const DEFAULT_INTERNAL_GRPC_PORT: u16 = 9090;
/// App/terminal-client listener: person tokens, authenticated by
/// TokenReview and authorized per request by SubjectAccessReview.
const DEFAULT_PERSON_GRPC_PORT: u16 = 9091;
/// Channel listener: `ch-<channel>` ServiceAccount tokens only.
const DEFAULT_CHANNEL_GRPC_PORT: u16 = 9092;

/// One `tonic` server future, boxed so the listener table can hold the
/// internal and per-class servers side by side.
type ServerFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), tonic::transport::Error>> + Send>,
>;

/// The credential a listener demands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerAuth {
    /// The harness link: a Kubernetes SA token, verified by TokenReview.
    Harness,
    /// An external gateway listener serving one credential class; see
    /// [`ListenerClass`].
    Class(ListenerClass),
}

/// Every socket the relay serves, paired with the credential it demands.
/// `main` builds its servers by walking this table.
fn listeners() -> Vec<(std::net::SocketAddr, ListenerAuth)> {
    [
        (DEFAULT_INTERNAL_GRPC_PORT, ListenerAuth::Harness),
        (
            DEFAULT_PERSON_GRPC_PORT,
            ListenerAuth::Class(ListenerClass::PersonToken),
        ),
        (
            DEFAULT_CHANNEL_GRPC_PORT,
            ListenerAuth::Class(ListenerClass::ChannelToken),
        ),
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

/// The services an external listener serves: the gateway and nothing else.
/// The harness-facing `RelayInternal` surface is served on the internal
/// listener only.
fn external_routes(state: Arc<GatewayState>) -> tonic::service::Routes {
    tonic::service::Routes::new(RelayGatewayServer::new(GatewayService::new(state)))
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
    let idp_issuer = std::env::var("IDP_ISSUER").unwrap_or_default();
    let idp_audience = std::env::var("IDP_AUDIENCE").unwrap_or_default();
    let idp_certificate_authority = std::env::var("IDP_CERTIFICATE_AUTHORITY").unwrap_or_default();
    let heartbeat_interval_secs: u64 = std::env::var("HEARTBEAT_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);

    let mut state = GatewayState::new_with_idp(
        Some(kube_client.clone()),
        namespace.clone(),
        idp_issuer,
        idp_audience,
        idp_certificate_authority,
    );
    state.set_heartbeat_interval(std::time::Duration::from_secs(heartbeat_interval_secs));
    let state = Arc::new(state);

    // Identity watch: the relay-identity ConfigMaps. An unsynced table is
    // empty, so it answers no agent list and resolves no handle —
    // serving in that state is silent and useless, so a timeout crashes
    // the process and lets the kubelet's restart backoff retry.
    shared::watcher_retry::spawn_and_await_ready(
        "relay-identity",
        "identity",
        kube_client.clone(),
        namespace.clone(),
        state.identity(),
        |client, ns, table, tx| async move {
            relay_controller::identity_watcher::watch_identities(client, &ns, table, tx).await
        },
    )
    .await?;

    let table = listeners();
    tracing::info!(
        listeners = ?table.iter().map(|(a, auth)| format!("{a} {auth:?}")).collect::<Vec<_>>(),
        "relay-controller listening"
    );

    let mut servers: Vec<ServerFuture> = Vec::with_capacity(table.len());

    for (addr, auth) in table {
        match auth {
            ListenerAuth::Harness => {
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
            ListenerAuth::Class(class) => {
                servers.push(Box::pin(
                    Server::builder()
                        .layer(class_layer(class))
                        .add_routes(external_routes(state.clone()))
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

    // `main` builds its servers by walking `listeners()`, wrapping the
    // person and channel entries in their per-class layer and leaving the
    // `Harness` entry bare. The tests below constrain that table rather
    // than describing it.

    #[test]
    fn channel_port_is_9092() {
        assert_eq!(DEFAULT_CHANNEL_GRPC_PORT, 9092);
    }

    // Drop the channel entry from the table and the count is two; give it 9091
    // or 9090 and the distinctness assertion reds. The relay would then have no
    // channel port at all while every other test in the suite stayed green.
    #[test]
    fn three_listeners_are_served_on_three_distinct_sockets() {
        let table = listeners();
        assert_eq!(table.len(), 3, "internal, person, and channel");
        let mut ports: Vec<u16> = table.iter().map(|(a, _)| a.port()).collect();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(ports.len(), 3, "listener ports must be distinct");
        assert!(ports.contains(&DEFAULT_INTERNAL_GRPC_PORT));
        assert!(ports.contains(&DEFAULT_PERSON_GRPC_PORT));
        assert!(ports.contains(&DEFAULT_CHANNEL_GRPC_PORT));
    }

    // The person and channel ports bind off loopback and are admitted by the
    // relay's ingress CNP, not by the pod boundary.
    //
    // Leave `format!("127.0.0.1:{DEFAULT_PERSON_GRPC_PORT}")` in place and the
    // tailnet channel, a separate Deployment, can never reach the relay. Nothing
    // else in the Rust suite observes the bind address.
    #[test]
    fn person_and_channel_listeners_bind_in_cluster_addresses() {
        for (addr, _) in listeners() {
            assert!(
                !addr.ip().is_loopback(),
                "{addr} binds loopback; no listener is same-pod any more"
            );
        }
    }

    /// The ports in `table` that demand `auth`.
    fn ports_for(table: &[(std::net::SocketAddr, ListenerAuth)], auth: ListenerAuth) -> Vec<u16> {
        table
            .iter()
            .filter(|(_, a)| *a == auth)
            .map(|(addr, _)| addr.port())
            .collect()
    }

    // The person socket carries the person-token class and the channel socket
    // the channel-token class; neither is the internal harness socket, so no
    // external listener accepts a harness credential.
    #[test]
    fn each_external_listener_carries_its_own_credential_class() {
        let table = listeners();
        assert_eq!(
            ports_for(&table, ListenerAuth::Class(ListenerClass::PersonToken)),
            vec![DEFAULT_PERSON_GRPC_PORT]
        );
        assert_eq!(
            ports_for(&table, ListenerAuth::Class(ListenerClass::ChannelToken)),
            vec![DEFAULT_CHANNEL_GRPC_PORT]
        );
        assert_eq!(
            ports_for(&table, ListenerAuth::Harness),
            vec![DEFAULT_INTERNAL_GRPC_PORT],
            "the internal port is the only harness socket"
        );
    }

    /// The gRPC status an external listener's routes answer for `path`, from
    /// the trailers-only response tonic sends for a refused call.
    async fn external_status_for(path: &str) -> Option<String> {
        use tower::ServiceExt;

        let state = Arc::new(GatewayState::new(None, "default".into()));
        let request = http::Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/grpc")
            .header("te", "trailers")
            // One empty, uncompressed gRPC message.
            .body(http_body_util::Full::new(bytes::Bytes::from_static(&[
                0, 0, 0, 0, 0,
            ])))
            .unwrap();
        let response = external_routes(state).oneshot(request).await.unwrap();
        response
            .headers()
            .get("grpc-status")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }

    // The external listeners carry the gateway only. Add `RelayInternalServer`
    // to `external_routes` and a person or channel token reaches the
    // harness-facing Subscribe surface from outside the cluster.
    #[tokio::test]
    async fn external_listeners_do_not_serve_relay_internal() {
        let unimplemented = (tonic::Code::Unimplemented as i32).to_string();
        assert_eq!(
            external_status_for("/relay.v1.RelayInternal/Subscribe")
                .await
                .as_deref(),
            Some(unimplemented.as_str()),
            "RelayInternal/Subscribe must be unimplemented on an external listener"
        );
        // The same routes do serve the gateway: the call reaches its handler,
        // which refuses the unclassed request for its own reason.
        assert_eq!(
            external_status_for("/relay.v1.RelayGateway/ListAgents")
                .await
                .as_deref(),
            Some((tonic::Code::PermissionDenied as i32).to_string().as_str()),
            "RelayGateway must be served on an external listener"
        );
    }

    #[test]
    fn internal_port_is_9090() {
        assert_eq!(DEFAULT_INTERNAL_GRPC_PORT, 9090);
    }

    #[test]
    fn person_port_is_9091() {
        assert_eq!(DEFAULT_PERSON_GRPC_PORT, 9091);
    }
}
