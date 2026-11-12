mod agent;
mod agent_state;
mod channel_stream_tools;
mod clients;
mod config;
mod conversation;
mod dispatch;
mod dispatch_link;
mod execution_log;
mod grpc_server;
mod healthz;
mod instructions;
mod job;
mod message_source;
mod model_config;
mod queue;
mod runtime_entrypoint;
mod runtime_tools;
#[cfg(test)]
mod test_doubles;
mod tool_approval;
mod tool_router;
mod turn;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use agent_state::AgentState;
use config::{HarnessConfig, Mode};
use dispatch::DispatchState;
use dispatch_link::{dispatch_verifier, MandateSource, ParentLink};
use shared::scheduling::SchedulingConfig;
use shared::toolset::ToolsetConfig;
use tokio::sync::RwLock;
use tonic::transport::Server;

/// The conversation root, a directory on the per-agent conversation PVC.
/// `conversation.log` and `execution.json` sit side by side under it.
const CONVERSATION_DIR: &str = "/var/lib/harness/conversations";

const HEALTHZ_PORT: u16 = 8080;
/// The relay-facing HarnessControl port. NetworkPolicy admits relay-ctrl alone
/// to it.
const RELAY_FORWARD_PORT: u16 = 9091;
/// The port a dispatch serves `DispatchLink` on. NetworkPolicy admits the
/// parent agent's harness alone to it.
const DISPATCH_LINK_PORT: u16 = 9092;

/// The port every capability-job pod serves its `ToolJob` gRPC server on. The
/// harness dials it, the per-agent headless Service publishes it, and the
/// pod readiness probe gates the per-pod A record on it.
const TOOL_JOB_PORT: u16 = 9090;

/// Boot-time writability guard for a log root. Fails boot if the dir cannot
/// be created or written.
fn probe_dir_writable(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("create conversation-log dir {}: {e}", dir.display()))?;
    let probe = dir.join(".write-probe");
    std::fs::write(&probe, b"probe").map_err(|e| {
        format!(
            "conversation-log dir {} is not writable: {e}",
            dir.display()
        )
    })?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = HarnessConfig::from_env().map_err(|e| format!("config error: {e}"))?;

    // The agent state is rebuilt from the PVC at boot, so a restart keeps
    // the conversation.
    let conversation_dir = PathBuf::from(CONVERSATION_DIR);
    probe_dir_writable(&conversation_dir)?;
    conversation::write_logrotate_conf(&conversation_dir, config.archives)?;
    let agent_state = Arc::new(AgentState::load(conversation_dir.clone(), config.archives).await?);
    tracing::info!(dir = %conversation_dir.display(), "conversation store ready");

    // In-process instructions reader over the mounted read-only instructions volume. Each
    // harness serves only its own agent's instructions (AGENTS.md, agents,
    // skills), read fresh on demand.
    let instructions = Arc::new(instructions::Instructions::new(
        config.instructions_root.clone(),
    ));
    tracing::info!(
        root = %config.instructions_root.display(),
        agent = %config.agent,
        "instructions reader ready"
    );

    // In-process tool-call dispatch: the harness spawns each tool's Job in its
    // own namespace and dials the pod. A kube client is required in cluster;
    // absent it (local dev) the dispatcher holds no client and the Job configs
    // default empty.
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

    // An agent talks to the relay. A dispatch talks to its parent agent's
    // harness, and its tool Jobs mount the parent's workspace.
    let (relay, link, workspace_agent, seam) = match &config.mode {
        Mode::Agent { relay_gateway_addr } => {
            let relay = clients::RelayClient::connect(relay_gateway_addr).await?;
            tracing::info!(addr = %relay_gateway_addr, "connected to relay gateway");
            let seam = tool_router::relay_seam(relay.clone());
            (Some(relay), None, config.agent.clone(), seam)
        }
        Mode::Dispatch { parent, .. } => {
            let client = kube_client
                .clone()
                .ok_or("a dispatch needs a kube client to check the parent's token")?;
            let link = ParentLink::new(
                parent.clone(),
                config.namespace.clone(),
                Arc::new(dispatch_verifier(client)),
            );
            let seam = tool_router::relay_seam(link.clone());
            (None, Some(link), parent.clone(), seam)
        }
    };
    let router = tool_router::ToolRouter::new(
        instructions.clone(),
        config.agent.clone(),
        Some(seam),
        agent_state.clone(),
    );

    let dispatch = DispatchState::new(
        kube_client,
        config.namespace.clone(),
        config.capability_service.clone(),
        config.agent.clone(),
        &workspace_agent,
        scheduling,
        toolset_config.clone(),
    );
    tracing::info!(
        namespace = %config.namespace,
        capability_service = %config.capability_service,
        "tool dispatch ready"
    );

    let tool_router: Arc<tool_router::ToolRouter> = Arc::new(
        router
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
    // and compaction drive model calls through this seam.
    let inference_factory: grpc_server::InferenceFactory = {
        let model = config.model.clone();
        let namespace = config.namespace.clone();
        let service = config.capability_service.clone();
        let agent = config.agent.clone();
        Arc::new(move || {
            Box::new(dispatch::InferenceDispatch::new(
                models.clone(),
                model.clone(),
                kube_for_inference.clone(),
                namespace.clone(),
                service.clone(),
                agent.clone(),
                scheduling_for_inference.clone(),
            ))
        })
    };
    let mut inference = inference_factory();
    let idle_gap = std::time::Duration::from_secs(config.idle_gap_secs);

    if let (Mode::Dispatch { path, mandate, .. }, Some(link)) = (&config.mode, link) {
        let addr: SocketAddr = ([0, 0, 0, 0], DISPATCH_LINK_PORT).into();
        let server = Server::builder()
            .add_service(harness_proto::dispatch_link_server::DispatchLinkServer::new(link.clone()))
            .serve(addr);
        tokio::spawn(async move {
            if let Err(e) = server.await {
                tracing::error!(error = %e, "dispatch link gRPC server exited");
            }
        });
        tracing::info!(dispatch_link = %addr, path = %path, "dispatch link listening");

        let mut source = MandateSource::new(path.clone(), mandate.clone());
        let mut relay_deliver = link.clone();
        let turns = runtime_entrypoint::message_loop(
            config.max_iterations,
            idle_gap,
            config.model.clone(),
            inference.as_mut(),
            &mut relay_deliver,
            &instructions,
            &config.agent,
            tool_router,
            agent_state,
            &mut source,
        );
        // The dispatch ends once the parent has taken its final answer.
        tokio::select! {
            res = turns => res?,
            () = link.answer_taken() => tracing::info!("final answer taken"),
        }
        return Ok(());
    }
    let relay = relay.ok_or("no relay client")?;
    let relay_subscribe = relay.clone();
    let mut relay_deliver = relay;

    let subscribed_flag = Arc::new(AtomicBool::new(false));
    tokio::spawn(healthz::serve(subscribed_flag.clone(), HEALTHZ_PORT));

    // One inbound gRPC listener: the relay-facing forward surface.
    // The harness serves no pod-facing surface — it dials each tool
    // pod directly, so a popped tool pod can open no socket back to the harness.
    let grpc_router_handle = tool_router.clone();
    let grpc_agent_state_handle = agent_state.clone();
    let relay_forward_addr: SocketAddr = ([0, 0, 0, 0], RELAY_FORWARD_PORT).into();

    tokio::spawn(async move {
        let svc = grpc_server::HarnessService::new(
            grpc_router_handle,
            grpc_agent_state_handle,
            inference_factory,
            idle_gap,
        );
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

    let mut source =
        message_source::SubscribeMessageSource::from_client(relay_subscribe, subscribed_flag);

    runtime_entrypoint::message_loop(
        config.max_iterations,
        idle_gap,
        config.model.clone(),
        inference.as_mut(),
        &mut relay_deliver,
        &instructions,
        &config.agent,
        tool_router,
        agent_state,
        &mut source,
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod probe_tests {
    #[test]
    fn probe_writable_dir_returns_ok_and_leaves_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        // A not-yet-existing subdir also exercises the create_dir_all branch.
        let target = dir.path().join("fresh");

        let result = super::probe_dir_writable(&target);
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
    // a read-only mount. The Err names the conversation-log dir and the
    // not-writable condition, so a pod log points an operator at the exact
    // failing mount.
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

        let msg = super::probe_dir_writable(&ro).expect_err(
            "an existing read-only dir must probe Err; create_dir_all alone would wrongly pass",
        );
        assert!(
            msg.contains("conversation-log"),
            "the error must name the conversation-log dir so the log identifies it, got {msg:?}"
        );
        assert!(
            msg.contains("is not writable"),
            "the error must state the not-writable condition, got {msg:?}"
        );
    }
}
