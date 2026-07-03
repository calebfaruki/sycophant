use clap::Parser;
use hangar_controller::audience_layer::RequiredAudienceLayer;
use hangar_controller::grpc::ControllerService;
use hangar_controller::state::ControllerState;
use hangar_proto::hangar_controller_server::HangarControllerServer;
use shared::auth::K8sTokenVerifier;
use std::sync::Arc;
use tonic::transport::Server;

/// Internal listener: K8s SA token via TokenReview. Bound `0.0.0.0`
/// so in-cluster workloads (LLM Job, transponder, syco-cli pods) can
/// reach it. The internet-facing gateway surface lives in
/// tightbeam-controller; hangar serves only in-cluster callers.
const DEFAULT_INTERNAL_GRPC_PORT: u16 = 9090;

#[derive(Parser)]
#[command(name = "hangar-controller", about = "Sycophant hangar controller")]
struct Cli {}

/// Build the audience-pair of auth verifiers for the internal gRPC
/// listener. K8s ServiceAccount tokens flow through ONE of these
/// verifiers depending on the requested gRPC method (the
/// `audience_layer` stamps `RequiredAudience` on each request and the
/// handler reads it to pick the transponder vs llm audience).
fn build_internal_verifier(
    kube_client: Option<&kube::Client>,
) -> Option<hangar_controller::grpc::InternalVerifierPair> {
    kube_client.map(|c| hangar_controller::grpc::InternalVerifierPair {
        transponder: Arc::new(K8sTokenVerifier::new(
            c.clone(),
            shared::auth::TRANSPONDER_HANGAR_AUDIENCE,
        )) as Arc<dyn shared::auth::TokenVerifier>,
        llm: Arc::new(K8sTokenVerifier::new(
            c.clone(),
            shared::auth::LLM_HANGAR_AUDIENCE,
        )) as Arc<dyn shared::auth::TokenVerifier>,
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

    let namespace = std::env::var("HANGAR_NAMESPACE").unwrap_or_else(|_| "default".into());

    let verifier = build_internal_verifier(Some(&kube_client));
    let controller_addr = std::env::var("HANGAR_CONTROLLER_ADDR")
        .unwrap_or_else(|_| format!("http://0.0.0.0:{DEFAULT_INTERNAL_GRPC_PORT}"));
    let llm_job_image = std::env::var("HANGAR_LLM_JOB_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/calebfaruki/hangar-llm-job:latest".into());

    let scheduling_file = std::env::var("HANGAR_SCHEDULING_FILE")
        .unwrap_or_else(|_| "/etc/sycophant/scheduling.yaml".into());
    let scheduling = shared::scheduling::SchedulingConfig::load_or_default(&scheduling_file, true)?;

    let state = Arc::new(ControllerState::new(
        Some(kube_client.clone()),
        namespace.clone(),
        controller_addr,
        llm_job_image,
        scheduling,
    ));

    {
        let (model_ready_tx, mut model_ready_rx) = tokio::sync::watch::channel(false);
        let (provider_ready_tx, mut provider_ready_rx) = tokio::sync::watch::channel(false);

        let model_state = state.clone();
        let model_ns = namespace.clone();
        let model_client = kube_client.clone();
        shared::watcher_retry::spawn_watcher_task("models", move || {
            let ns = model_ns.clone();
            let client = model_client.clone();
            let state = model_state.clone();
            let tx = model_ready_tx.clone();
            async move { hangar_controller::watcher::watch_models(client, &ns, state, tx).await }
        });

        let provider_state = state.clone();
        let provider_ns = namespace.clone();
        let provider_client = kube_client.clone();
        shared::watcher_retry::spawn_watcher_task("providers", move || {
            let ns = provider_ns.clone();
            let client = provider_client.clone();
            let state = provider_state.clone();
            let tx = provider_ready_tx.clone();
            async move { hangar_controller::watcher::watch_providers(client, &ns, state, tx).await }
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

    // Keepalive: reconcile existing LLM Jobs into the per-model state,
    // then run the 30s idle sweep. Must fire AFTER the watcher initial
    // sync so `bump_model_activity` finds populated slots for adopted
    // Jobs; an orphaned Job whose Model CR is gone gets reaped on the
    // first sweep.
    {
        let keepalive_state = state.clone();
        let keepalive_client = kube_client.clone();
        let keepalive_ns = namespace.clone();
        tokio::spawn(async move {
            if let Err(e) = hangar_controller::keepalive::reconcile_active_jobs(
                &keepalive_client,
                &keepalive_ns,
                &keepalive_state,
            )
            .await
            {
                tracing::error!(
                    error = %e,
                    "reconcile_active_jobs failed; cleanup loop will operate on partial state"
                );
            }
            hangar_controller::keepalive::cleanup_loop(keepalive_state).await;
        });
    }

    // Reactive LLM-job watch: fail a turn the instant its worker Job goes
    // terminal or is deleted, rather than waiting for the idle sweep. Uses
    // the existing batch/jobs:watch RBAC grant — no new permission.
    {
        let watch_state = state.clone();
        let watch_client = kube_client.clone();
        let watch_ns = namespace.clone();
        shared::watcher_retry::spawn_watcher_task("llm-jobs", move || {
            let client = watch_client.clone();
            let ns = watch_ns.clone();
            let state = watch_state.clone();
            async move { hangar_controller::keepalive::watch_llm_jobs(client, &ns, state).await }
        });
    }

    let internal_service = ControllerService::internal(state.clone(), verifier);

    let internal_addr = format!("0.0.0.0:{DEFAULT_INTERNAL_GRPC_PORT}").parse()?;

    let (health_reporter, internal_health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<HangarControllerServer<ControllerService>>()
        .await;

    let internal_reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(hangar_proto::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    tracing::info!(
        internal = %internal_addr,
        "hangar-controller listening on internal in-cluster listener"
    );

    Server::builder()
        .layer(RequiredAudienceLayer)
        .add_service(internal_reflection)
        .add_service(internal_health_service)
        .add_service(HangarControllerServer::new(internal_service))
        .serve(internal_addr)
        .await?;

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
    fn internal_port_is_9090() {
        assert_eq!(DEFAULT_INTERNAL_GRPC_PORT, 9090);
    }
}
