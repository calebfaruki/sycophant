mod agent;
mod channel_stream_tools;
mod clients;
mod config;
mod dispatch;
mod execution_log;
// `conversation` is the agent-history library surface (log store,
// frontmatter, snapshots). Exposed as a public module so its API, exercised
// by unit tests, isn't dead-code-flagged in this bin-shaped crate.
pub mod conversation;
mod grpc_server;
mod healthz;
mod job;
// The per-agent instructions reader (AGENTS.md / agents / skills). Public so
// the crate's integration tests can exercise the reader's error asymmetry
// directly.
pub mod instructions;
mod message_source;
mod model_config;
mod registry;
mod runtime_entrypoint;
mod runtime_tools;
#[cfg(test)]
pub(crate) mod test_doubles;
mod tool_approval;
mod tool_router;
mod turn;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use config::HarnessConfig;
use conversation::{ConversationStoreFactory, LocalFsFactory};
use dispatch::DispatchState;
use message_source::MessageSource;
use registry::ConversationRegistry;
use shared::scheduling::SchedulingConfig;
use shared::toolset::ToolsetConfig;
use tokio::sync::RwLock;
use tonic::transport::Server;

/// Default on-disk root for conversation event logs. Mounted from the
/// per-agent conversation PVC by the chart. Each conversation directory
/// under it holds both `conversation.json` and its `execution.json` +
/// `blobs/`, so the execution log needs no separate root.
const DEFAULT_CONVERSATION_DIR: &str = "/var/lib/harness/conversations";

const HEALTHZ_PORT: u16 = 8080;
/// Relay-facing forward surface (HarnessControl: WatchTools, DispatchTool,
/// and the conversation methods). Relay forwards registered-user requests
/// here. Split onto its own port so L4 NetworkPolicy can admit relay-ctrl
/// alone, never a capability-job — the two surfaces carry different trust
/// and L4 cannot tell two gRPC services apart on one port.
const RELAY_FORWARD_PORT: u16 = 9091;

/// The port every capability-job pod serves its `ToolJob` gRPC server on. The
/// harness dials it, the per-agent headless Service publishes it, and the
/// pod readiness probe gates the per-pod A record on it.
const TOOL_JOB_PORT: u16 = 9090;

/// Boot-time writability guard for a log root. Fails fast if the chart drift
/// left the dir missing or read-only, turning a silent per-call WARN into a
/// non-zero process exit.
fn probe_dir_writable(label: &str, dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("create {label} dir {}: {e}", dir.display()))?;
    let probe = dir.join(".write-probe");
    std::fs::write(&probe, b"probe")
        .map_err(|e| format!("{label} dir {} is not writable: {e}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = HarnessConfig::from_env().map_err(|e| format!("config error: {e}"))?;

    // Conversation log store + registry. The harness is the sole author
    // of its agent's history; rebuild the in-memory registry from the
    // PVC at boot so restart preserves thread context.
    let conversation_dir = std::env::var("HARNESS_CONVERSATION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONVERSATION_DIR));
    probe_dir_writable("conversation-log", &conversation_dir)?;
    let factory: Arc<dyn ConversationStoreFactory> =
        Arc::new(LocalFsFactory::new(conversation_dir.clone()));
    let registry = Arc::new(ConversationRegistry::new(factory));
    if let Err(e) = registry.rebuild_from_disk().await {
        tracing::warn!(error = %e, "conversation registry rebuild failed; continuing empty");
    }
    tracing::info!(dir = %conversation_dir.display(), "conversation store ready");

    let relay = clients::RelayClient::connect(&config.relay_gateway_addr).await?;
    let relay_subscribe = relay.clone();
    let mut relay_deliver = relay.clone();
    tracing::info!(addr = %config.relay_gateway_addr, "connected to relay gateway");

    // In-process instructions reader over the mounted read-only instructions volume. Each
    // harness serves only its own agent's instructions (AGENTS.md, agents,
    // skills), read fresh on demand — no separate instructions-serving pod.
    let instructions = Arc::new(instructions::Instructions::new(
        config.instructions_root.clone(),
    ));
    tracing::info!(
        root = %config.instructions_root.display(),
        agent = %config.agent,
        "instructions reader ready"
    );

    // In-process tool-call dispatch. The harness spawns each tool's Job into its
    // own namespace and serves the pod its assignment and result stream, rather
    // than round-tripping to the toolset controller. A kube client is required
    // in cluster; absent it (local dev) the dispatcher holds no client and the
    // Job configs default empty.
    let kube_client = match shared::try_init_kube_client().await {
        Ok(client) => Some(client),
        Err(e) => {
            tracing::warn!(error = %e, "no kube client; tool dispatch will not spawn Jobs");
            None
        }
    };
    // The live toolset table, empty at boot; the toolset watcher below fills it.
    let toolset_config = Arc::new(RwLock::new(ToolsetConfig::empty()));
    let scheduling =
        SchedulingConfig::load_or_default(&config.scheduling_file, kube_client.is_some())
            .map_err(|e| format!("scheduling config: {e}"))?;
    // The live model table, empty at boot; the model watcher below fills it.
    let models = Arc::new(RwLock::new(model_config::ModelTable::default()));
    // The inference dispatcher shares the tool dispatcher's kube client and
    // scheduling, so clone them before `DispatchState::new` consumes the
    // originals.
    let kube_for_inference = kube_client.clone();
    let kube_for_toolset = kube_client.clone();
    let kube_for_models = kube_client.clone();
    let scheduling_for_inference = scheduling.clone();
    let dispatch = DispatchState::new(
        kube_client,
        config.namespace.clone(),
        config.capability_service.clone(),
        config.agent.clone(),
        scheduling,
        toolset_config.clone(),
    );
    tracing::info!(
        namespace = %config.namespace,
        capability_service = %config.capability_service,
        "tool dispatch ready"
    );

    // The toolset execution log is harness-authored and toolset-unwritable,
    // one `execution.json` per conversation in that conversation's directory on
    // the harness's PVC. The router derives each writer from the registry;
    // there is no separate execution-log root.
    let tool_router: Arc<tool_router::ToolRouter> = Arc::new(
        tool_router::ToolRouter::new(
            instructions.clone(),
            config.agent.clone(),
            Some(relay),
            registry.clone(),
        )
        .with_dispatch(dispatch.clone())
        .with_toolset_scope(config.toolsets.clone(), config.toolset_secrets.clone()),
    );

    // The live dispatch table, which also rebuilds the router's toolset tools on
    // every change. Only in cluster: local dev holds no kube client and
    // dispatches no Jobs.
    if let Some(client) = kube_for_toolset {
        let router = tool_router.clone();
        // An unsynced table dispatches no toolset, so a timeout crashes the
        // process and lets the kubelet's restart backoff retry rather than
        // answer every call with "not found".
        shared::watcher_retry::spawn_and_await_ready(
            "toolset",
            "toolset",
            client,
            config.namespace.clone(),
            toolset_config.clone(),
            move |client, ns, table, tx| {
                let router = router.clone();
                async move {
                    shared::configmap_watcher::watch_configmaps(
                        client,
                        &ns,
                        "sycophant.md/type=toolset",
                        "toolset",
                        table,
                        tx,
                        move |cms| {
                            let (config, errors) = ToolsetConfig::merge_configmaps(cms.values());
                            for warning in router.apply_toolsets(&config) {
                                tracing::warn!(reason = %warning, "toolset excluded");
                            }
                            (config, errors)
                        },
                    )
                    .await
                }
            },
        )
        .await?;
    }

    // The live model table. A model ConfigMap added, changed or removed takes
    // effect on the next turn.
    if let Some(client) = kube_for_models {
        shared::watcher_retry::spawn_and_await_ready(
            "model",
            "model",
            client,
            config.namespace.clone(),
            models.clone(),
            |client, ns, table, tx| async move {
                shared::configmap_watcher::watch_configmaps(
                    client,
                    &ns,
                    "sycophant.md/type=model",
                    "model",
                    table,
                    tx,
                    |cms| model_config::ModelTable::merge_configmaps(cms.values()),
                )
                .await
            },
        )
        .await?;
    }

    // The harness dials the model directly: a warm local Service in-process, or
    // a per-call inference-runtime Job it creates and dials. The message loop
    // drives turns through this seam rather than the toolset controller.
    let mut inference = dispatch::InferenceDispatch::new(
        models,
        config.model.clone(),
        kube_for_inference,
        config.namespace.clone(),
        config.capability_service.clone(),
        config.agent.clone(),
        scheduling_for_inference,
    );

    let subscribed_flag = Arc::new(AtomicBool::new(false));
    tokio::spawn(healthz::serve(subscribed_flag.clone(), HEALTHZ_PORT));

    // One inbound gRPC listener: the relay-facing forward surface.
    // Authentication is structural: NetworkPolicy admits relay-ctrl alone to
    // this port. The harness serves no pod-facing surface — it dials each tool
    // pod directly, so a popped tool pod can open no socket back to the harness.
    let grpc_router_handle = tool_router.clone();
    let grpc_registry_handle = registry.clone();
    let relay_forward_addr: SocketAddr = ([0, 0, 0, 0], RELAY_FORWARD_PORT).into();

    // Relay-facing forward surface (HarnessControl). Ingress: relay-ctrl only.
    tokio::spawn(async move {
        let svc = grpc_server::HarnessService::new(grpc_router_handle, grpc_registry_handle);
        let server = Server::builder()
            .add_service(harness_proto::harness_control_server::HarnessControlServer::new(svc))
            .serve(relay_forward_addr);
        if let Err(e) = server.await {
            tracing::error!(error = %e, "harness relay-forward gRPC server exited");
        }
    });
    tracing::info!(
        relay_forward = %relay_forward_addr,
        "harness gRPC server listening"
    );

    let mut source: Box<dyn MessageSource> = Box::new(
        message_source::SubscribeMessageSource::from_client(relay_subscribe, subscribed_flag),
    );

    runtime_entrypoint::message_loop(
        config.max_iterations,
        std::time::Duration::from_secs(config.idle_gap_secs),
        config.model.clone(),
        &mut inference,
        &mut relay_deliver,
        &instructions,
        &config.agent,
        tool_router,
        registry,
        source.as_mut(),
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod probe_tests {
    // Boot-time writability probe. The chart mounts a writable PVC at the
    // harness log root; a chart drift that omits the mount leaves the dir
    // read-only, and today that only surfaces as a per-call WARN while the pod
    // reports green. `probe_dir_writable` is the fail-fast guard that turns that
    // silent breakage into a non-zero process exit at boot.

    // A fresh, writable target probes Ok AND leaves no `.write-probe` residue on
    // the PVC.
    #[test]
    fn probe_writable_dir_returns_ok_and_leaves_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        // A not-yet-existing subdir also exercises the create_dir_all branch.
        let target = dir.path().join("fresh");

        let result = super::probe_dir_writable("conversation-log", &target);
        assert!(
            result.is_ok(),
            "a fresh writable dir must probe Ok, got {result:?}"
        );
        assert!(
            !target.join(".write-probe").exists(),
            "the probe must remove its .write-probe file, leaving no residue on the PVC"
        );
    }

    // An existing but read-only directory probes Err. `create_dir_all` alone
    // returns Ok on an already-present dir, so only the probe-file write catches
    // a read-only mount. The Err names the label and the not-writable
    // condition, so a pod log points an operator at the exact failing mount.
    //
    // Guards cleanly under root, where mode bits are bypassed and the read-only
    // premise does not hold.
    #[cfg(unix)]
    #[test]
    fn probe_existing_read_only_dir_returns_err() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Root bypasses mode bits; if a direct write into the dir succeeds we are
        // privileged and the read-only premise is void — skip rather than pass.
        let canary = ro.join(".root-canary");
        if std::fs::write(&canary, b"x").is_ok() {
            let _ = std::fs::remove_file(&canary);
            eprintln!("skipping probe_existing_read_only_dir_returns_err: running as root");
            return;
        }

        let msg = super::probe_dir_writable("conversation-log", &ro).expect_err(
            "an existing read-only dir must probe Err; create_dir_all alone would wrongly pass",
        );
        assert!(
            msg.contains("conversation-log"),
            "the error must name the label so the log identifies which mount, got {msg:?}"
        );
        assert!(
            msg.contains("is not writable"),
            "the error must state the not-writable condition, got {msg:?}"
        );
    }
}
