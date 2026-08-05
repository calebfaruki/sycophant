mod agent;
mod channel_tools;
mod clients;
mod config;
mod execution_log;
// `conversation` is the workspace-history library surface (log store,
// scopes, frontmatter, snapshots). Exposed as a public module so its
// intentionally broad API — exercised by unit tests and reserved for the
// delegate-scoping path — isn't dead-code-flagged in this bin-shaped crate.
pub mod conversation;
mod grpc_server;
mod healthz;
// The per-workspace kernel reader (AGENTS.md / personas / skills). Public so
// the crate's integration tests can exercise the reader's error asymmetry
// directly.
pub mod kernel;
mod message_source;
mod registry;
mod runtime_entrypoint;
mod runtime_tools;
#[cfg(test)]
pub(crate) mod test_doubles;
mod tool_router;
mod turn;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use config::HarnessConfig;
use conversation::{ConversationStoreFactory, LocalFsFactory};
use message_source::MessageSource;
use registry::ConversationRegistry;
use tonic::transport::Server;

/// Default on-disk root for conversation event logs. Mounted from the
/// per-workspace conversation PVC by the chart. Each conversation directory
/// under it holds both `conversation.json` and its `execution.json` +
/// `blobs/`, so the execution log needs no separate root.
const DEFAULT_CONVERSATION_DIR: &str = "/var/lib/harness/conversations";

const HEALTHZ_PORT: u16 = 8080;
/// Inbound gRPC port for relay → harness forwarding (WatchTools,
/// CallTool). Mirrors the toolset controller pattern.
const GRPC_PORT: u16 = 9090;

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
    // of its workspace's history; rebuild the in-memory registry from the
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

    let toolset = clients::ToolsetClient::connect(&config.toolset_addr).await?;
    tracing::info!(addr = %config.toolset_addr, "connected to toolset controller");

    let relay = clients::RelayClient::connect(&config.relay_gateway_addr).await?;
    let relay_subscribe = relay.clone();
    let mut relay_deliver = relay.clone();
    tracing::info!(addr = %config.relay_gateway_addr, "connected to relay gateway");

    // Three ToolsetClient handles share a single underlying HTTP/2 connection
    // (tonic Channels multiplex): the message loop drives turns, the router
    // dispatches tool calls (needs `&mut self`), and the background
    // `watch_toolset_tools` task holds the tool-catalog stream open. The Rust
    // borrow constraint requires distinct values; the network sees one
    // connection.
    let mut toolset_for_turns = toolset.clone();
    let toolset_for_router = toolset.clone();
    let toolset_for_watch = toolset;

    // In-process kernel reader over the mounted read-only kernel volume. Each
    // harness serves only its own workspace's kernel (AGENTS.md, personas,
    // skills), read fresh on demand — no separate kernel-serving pod.
    let kernel = Arc::new(kernel::Kernel::new(config.kernel_root.clone()));
    tracing::info!(
        root = %config.kernel_root.display(),
        workspace = %config.workspace,
        "kernel reader ready"
    );

    // The toolset execution log is harness-authored and toolset-unwritable,
    // one `execution.json` per conversation in that conversation's directory on
    // the harness's PVC. The router derives each writer from the registry;
    // there is no separate execution-log root.
    let tool_router = Arc::new(tool_router::ToolRouter::new(
        kernel.clone(),
        config.workspace.clone(),
        Some(toolset_for_router),
        Some(relay),
        registry.clone(),
    ));

    let mut initial_waits = Vec::new();

    {
        let router_for_watch = tool_router.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        initial_waits.push(rx);
        tokio::spawn(async move {
            tool_router::watch_toolset_tools(toolset_for_watch, router_for_watch, Some(tx)).await;
        });
    }

    // Block message processing until the toolset tool set delivers its first
    // snapshot. Kernel-served tools (Skill/Skills) and the primary persona are
    // read in-process on demand, so they need no startup barrier.
    for rx in initial_waits {
        let _ = rx.await;
    }

    let subscribed_flag = Arc::new(AtomicBool::new(false));
    tokio::spawn(healthz::serve(subscribed_flag.clone(), HEALTHZ_PORT));

    // Start the inbound gRPC server for relay-controller forwards.
    // Authentication is structural in v1: NetworkPolicy restricts
    // ingress to relay-ctrl. TokenReview-based audience check is a
    // follow-up; the audience constant + chart mounts are in place.
    let grpc_router_handle = tool_router.clone();
    let grpc_registry_handle = registry.clone();
    let grpc_addr: SocketAddr = ([0, 0, 0, 0], GRPC_PORT).into();
    tokio::spawn(async move {
        let svc = grpc_server::HarnessService::new(grpc_router_handle, grpc_registry_handle);
        let server = Server::builder()
            .add_service(toolset_proto::harness_control_server::HarnessControlServer::new(svc))
            .serve(grpc_addr);
        if let Err(e) = server.await {
            tracing::error!(error = %e, "harness gRPC server exited");
        }
    });
    tracing::info!(addr = %grpc_addr, "harness gRPC server listening");

    let mut source: Box<dyn MessageSource> = Box::new(
        message_source::SubscribeMessageSource::from_client(relay_subscribe, subscribed_flag),
    );

    runtime_entrypoint::message_loop(
        config.max_iterations,
        std::time::Duration::from_secs(config.idle_gap_secs),
        &mut toolset_for_turns,
        &mut relay_deliver,
        &kernel,
        &config.workspace,
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
    //
    // Materiality: dropping the `remove_file` step leaves `.write-probe` behind,
    // reding the residue assertion. The dual assertion (Ok AND absence of the
    // residue file) is what makes this load-bearing rather than a bare
    // return-type restatement.
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
    // a read-only mount.
    //
    // Materiality: an impl that only `create_dir_all`s and never writes the probe
    // file wrongly returns Ok here — this reds it. Guards cleanly under root,
    // where mode bits are bypassed and the read-only premise does not hold.
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

        let result = super::probe_dir_writable("conversation-log", &ro);
        assert!(
            result.is_err(),
            "an existing read-only dir must probe Err; create_dir_all alone would wrongly pass"
        );
    }

    // The Err on the read-only case names the label and the not-writable
    // condition, so a pod log points an operator at the exact failing mount.
    //
    // Materiality: an impl that returns a bare/opaque error (or omits the label)
    // reds these substring assertions. Distinct from the presence-of-Err test
    // above — this pins the diagnostic content, not merely that it failed.
    #[cfg(unix)]
    #[test]
    fn probe_read_only_err_message_names_label_and_not_writable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();

        let canary = ro.join(".root-canary");
        if std::fs::write(&canary, b"x").is_ok() {
            let _ = std::fs::remove_file(&canary);
            eprintln!(
                "skipping probe_read_only_err_message_names_label_and_not_writable: running as root"
            );
            return;
        }

        let msg = super::probe_dir_writable("conversation-log", &ro)
            .expect_err("an existing read-only dir must probe Err");
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
