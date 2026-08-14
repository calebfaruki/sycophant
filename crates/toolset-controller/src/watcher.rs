use std::time::Duration;

use kube::Client;
use tracing::{error, info, warn};

use crate::crd::ToolsetEntry;
use crate::job::{build_discovery_job, create_job};
use crate::registry::{DiscoveredTool, RegistryError};
use crate::state::{ControllerState, ToolsetConfig, WorkspaceBindings};
use shared::scheduling::SchedulingConfig;

/// Backoff schedule for toolset tool discovery: initial attempt + five backoffs
/// totalling ~15.5 s. Anything unresolved by then is a hard failure that
/// excludes the toolset from controller state.
const DISCOVERY_BACKOFF_MS: &[u64] = &[500, 1000, 2000, 4000, 8000];

/// Run `attempt` on the `DISCOVERY_BACKOFF_MS` schedule. Retries errors
/// `is_retryable` accepts; a deterministic error bails on the attempt that
/// produced it. `subject` and `operation` name the work in the retry log.
async fn retry_backoff<T, E, F, Fut>(
    subject: &str,
    operation: &str,
    is_retryable: fn(&E) -> bool,
    mut attempt: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut last_err: Option<E> = None;
    let total = DISCOVERY_BACKOFF_MS.len() + 1;
    for (attempt_idx, delay_ms) in std::iter::once(0_u64)
        .chain(DISCOVERY_BACKOFF_MS.iter().copied())
        .enumerate()
    {
        if attempt_idx > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(e) if !is_retryable(&e) => return Err(e),
            Err(e) => {
                warn!(
                    subject,
                    operation,
                    attempt = attempt_idx + 1,
                    total,
                    error = %e,
                    "retryable error"
                );
                last_err = Some(e);
            }
        }
    }
    // The loop only continues after capturing a retryable error, so exhaustion
    // always has one to return.
    Err(last_err.expect("an exhausted retry has captured its last error"))
}

/// Run `fetch` against `image` with bounded retry. Retries `RegistryError`s
/// where `is_retryable()` is true; deterministic errors bail on the first
/// attempt.
pub async fn retry_discovery<F, Fut>(
    image: &str,
    mut fetch: F,
) -> Result<Vec<DiscoveredTool>, RegistryError>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<DiscoveredTool>, RegistryError>>,
{
    retry_backoff(image, "discovery", RegistryError::is_retryable, || {
        fetch(image.to_string())
    })
    .await
}

/// A failed attempt to report a discovered tool set to the controller.
#[derive(Debug)]
pub enum ReportError {
    /// The connection never came up: refused, reset, or DNS. The controller's
    /// Service publishes no endpoint until its readiness probe passes, so a
    /// discovery Job spawned at controller boot always sees this first.
    Transport(tonic::transport::Error),
    /// The controller answered and refused.
    Rpc(tonic::Status),
}

impl ReportError {
    /// Transport failures are always worth another attempt. An answered call is
    /// only worth retrying when the controller said it was not ready:
    /// `InvalidArgument` (a malformed tool set), `Unauthenticated`, and
    /// `PermissionDenied` are decided, and re-sending burns the Job's deadline.
    pub fn is_retryable(&self) -> bool {
        match self {
            ReportError::Transport(_) => true,
            ReportError::Rpc(status) => matches!(
                status.code(),
                tonic::Code::Unavailable | tonic::Code::DeadlineExceeded
            ),
        }
    }
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReportError::Transport(e) => write!(f, "transport: {e}"),
            ReportError::Rpc(s) => write!(f, "rpc: {s}"),
        }
    }
}

impl std::error::Error for ReportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReportError::Transport(e) => Some(e),
            ReportError::Rpc(s) => Some(s),
        }
    }
}

/// Send `report` for `toolset` with bounded retry. Each attempt must reconnect:
/// the failure this exists for is the connect itself, against a controller
/// whose Service has no ready endpoint yet.
pub async fn retry_report<F, Fut>(toolset: &str, report: F) -> Result<(), ReportError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), ReportError>>,
{
    retry_backoff(toolset, "report", ReportError::is_retryable, report).await
}

/// Spawns the ephemeral discovery Job that reads a toolset image's tool label
/// off the registry and reports it back over `ReportDiscoveredTools`. The
/// controller holds no registry egress, so the reach lives entirely in the Job.
/// Trait so reconcile can be unit-tested against a fake that observes the spawn
/// without a live cluster.
#[tonic::async_trait]
pub trait DiscoverySpawner: Send + Sync {
    /// Create a discovery Job for `toolset_name` reading `image`, running under
    /// `workspace`'s ServiceAccount. Returns `Err` if the Job could not be
    /// created. On `Ok`, the tools arrive asynchronously via the report handler.
    async fn spawn(&self, toolset_name: &str, image: &str, workspace: &str) -> Result<(), String>;
}

/// Live spawner: builds the discovery Job and creates it via the K8s API.
pub struct K8sDiscoverySpawner {
    pub client: Client,
    pub namespace: String,
    pub controller_addr: String,
    pub scheduling: SchedulingConfig,
}

#[tonic::async_trait]
impl DiscoverySpawner for K8sDiscoverySpawner {
    async fn spawn(&self, toolset_name: &str, image: &str, workspace: &str) -> Result<(), String> {
        let job = build_discovery_job(
            toolset_name,
            image,
            &self.namespace,
            &self.controller_addr,
            workspace,
            &self.scheduling,
        );
        create_job(&self.client, &self.namespace, &job)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Pure reconcile decision for one toolset, derived from its image and the
/// workspaces bound to it. No I/O, so the branch selection is unit-testable.
#[derive(Debug, PartialEq)]
pub(crate) enum DiscoveryPlan {
    /// The toolset declares an image and at least one workspace is bound: spawn
    /// discovery under that workspace's ServiceAccount.
    Spawn { workspace: String },
    /// The toolset declares no image: it serves no tools.
    NoImage,
    /// The toolset declares an image but no workspace is bound, so no pod can
    /// mint the worker token to run discovery and nothing could call the tools.
    NoWorkspace,
}

pub(crate) fn plan_discovery(
    image: Option<&str>,
    workspaces_for_toolset: &[String],
) -> DiscoveryPlan {
    match image {
        None => DiscoveryPlan::NoImage,
        Some(_) => match workspaces_for_toolset.first() {
            Some(ws) => DiscoveryPlan::Spawn {
                workspace: ws.clone(),
            },
            None => DiscoveryPlan::NoWorkspace,
        },
    }
}

/// Reconcile one toolset: decide, then spawn the discovery Job if warranted.
/// The controller performs NO in-process registry pull — discovery reach lives
/// in the spawned Job, which reports its tools back over the report RPC.
///
/// On `Ok` the toolset entry is registered so `begin_tool_call` can resolve its
/// image and secrets once the reported tools land. On spawn failure the
/// toolset's tools are left unregistered and `Err` is returned.
pub(crate) async fn reconcile_toolset(
    state: &ControllerState,
    spawner: &dyn DiscoverySpawner,
    bindings: &WorkspaceBindings,
    entry: &ToolsetEntry,
    name: &str,
) -> Result<(), String> {
    let workspaces = bindings.workspaces_for_toolset(name);
    match plan_discovery(entry.image.as_deref(), &workspaces) {
        DiscoveryPlan::NoImage => {
            state.remove_tools_for_toolset(name).await;
            state.set_toolset(name.to_string(), entry.clone()).await;
            Ok(())
        }
        DiscoveryPlan::NoWorkspace => {
            info!(toolset = %name, "toolset has an image but no bound workspace; skipping discovery");
            state.set_toolset(name.to_string(), entry.clone()).await;
            Ok(())
        }
        DiscoveryPlan::Spawn { workspace } => {
            let image = entry.image.as_deref().expect("Spawn plan implies an image");
            match spawner.spawn(name, image, &workspace).await {
                Ok(()) => {
                    info!(toolset = %name, %image, %workspace, "spawned discovery Job");
                    state.set_toolset(name.to_string(), entry.clone()).await;
                    Ok(())
                }
                Err(e) => {
                    error!(toolset = %name, %image, error = %e, "failed to spawn discovery Job");
                    state.remove_tools_for_toolset(name).await;
                    Err(e)
                }
            }
        }
    }
}

/// Drive discovery once at startup, one pass over the loaded toolset config.
/// The config is read at boot and never watched: changing it rolls the pod.
pub async fn reconcile_toolsets(
    state: &ControllerState,
    spawner: &dyn DiscoverySpawner,
    bindings: &WorkspaceBindings,
    toolsets: &ToolsetConfig,
) {
    for (name, entry) in toolsets.entries() {
        if let Err(e) = reconcile_toolset(state, spawner, bindings, entry, name).await {
            warn!(toolset = %name, error = %e, "toolset discovery failed; its tools stay unregistered");
        }
    }
    info!(
        toolset_count = state.toolset_count().await,
        "toolset config reconciled"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::DiscoveredTool;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ok_tools() -> Vec<DiscoveredTool> {
        vec![DiscoveredTool {
            name: "test-cmd".into(),
            description: Some("test".into()),
            args: vec![],
        }]
    }

    #[tokio::test(start_paused = true)]
    async fn retry_discovery_returns_ok_on_first_attempt() {
        let calls = AtomicUsize::new(0);
        let result = retry_discovery("img", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(ok_tools()) }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_discovery_returns_ok_after_two_failures() {
        let calls = AtomicUsize::new(0);
        let result = retry_discovery("img", |_| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(RegistryError::UnexpectedResponse(format!("fail {n}")))
                } else {
                    Ok(ok_tools())
                }
            }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_discovery_returns_err_after_six_total_attempts() {
        let calls = AtomicUsize::new(0);
        let result = retry_discovery("img", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(RegistryError::UnexpectedResponse("always fails".into())) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            DISCOVERY_BACKOFF_MS.len() + 1,
            "should run initial + every backoff entry"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_discovery_does_not_retry_invalid_label() {
        let calls = AtomicUsize::new(0);
        let result = retry_discovery("img", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(RegistryError::InvalidLabel("bad json".into())) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_discovery_does_not_retry_invalid_image_ref() {
        let calls = AtomicUsize::new(0);
        let result = retry_discovery("img", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(RegistryError::InvalidImageRef("bad ref".into())) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_discovery_unexpected_then_invalid_label_bails_on_second_call() {
        let calls = AtomicUsize::new(0);
        let result = retry_discovery("img", |_| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    Err(RegistryError::UnexpectedResponse("transient".into()))
                } else {
                    Err(RegistryError::InvalidLabel("bad".into()))
                }
            }
        })
        .await;
        assert!(matches!(result, Err(RegistryError::InvalidLabel(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    // ---- Report retry (discovery Job -> controller) ----

    #[tokio::test(start_paused = true)]
    async fn retry_report_retries_unavailable_then_succeeds() {
        let calls = AtomicUsize::new(0);
        let result = retry_report("stdlib", || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(ReportError::Rpc(tonic::Status::unavailable(
                        "no ready endpoint",
                    )))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "the report must survive the controller's readiness window, not die on the first refused connect"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_report_does_not_retry_invalid_argument() {
        let calls = AtomicUsize::new(0);
        let result = retry_report("stdlib", || {
            calls.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(ReportError::Rpc(tonic::Status::invalid_argument(
                    "unknown arg type",
                )))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a payload the controller already rejected must not be re-sent"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_report_exhausts_after_six_attempts() {
        let calls = AtomicUsize::new(0);
        let result = retry_report("stdlib", || {
            calls.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(ReportError::Rpc(tonic::Status::unavailable(
                    "still no endpoint",
                )))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            DISCOVERY_BACKOFF_MS.len() + 1,
            "should run initial + every backoff entry"
        );
    }

    /// A real `tonic::transport::Error`. `Endpoint::from_shared` rejects a
    /// malformed URI without touching the network, which is the only way to
    /// build the type: it has no public constructor. The inner cause is an
    /// invalid URI rather than a refused connect, which is the point — every
    /// `Transport` variant is retryable regardless of cause.
    fn transport_error() -> tonic::transport::Error {
        tonic::transport::Endpoint::from_shared("not a uri")
            .expect_err("a malformed URI must not parse")
    }

    #[test]
    fn transport_failures_are_retryable() {
        assert!(ReportError::Transport(transport_error()).is_retryable());
    }

    #[tokio::test(start_paused = true)]
    async fn retry_report_retries_transport_failures_to_exhaustion() {
        let calls = AtomicUsize::new(0);
        let result = retry_report("stdlib", || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(ReportError::Transport(transport_error())) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            DISCOVERY_BACKOFF_MS.len() + 1,
            "a refused connect is the failure this retry exists for; treating it as terminal reinstates the boot-time race"
        );
    }

    // ---- Reconcile seam (discovery-Job spawn) ----

    use std::collections::HashMap;
    use std::sync::Arc;

    /// Observes the discovery-Job spawn without a live cluster, so reconcile's
    /// two guarantees get real unit coverage: (a) the controller performs NO
    /// in-process registry pull on apply, and (b) a failed spawn leaves the
    /// toolset unregistered and errors (so the caller marks it unready).
    struct FakeSpawner {
        calls: std::sync::Mutex<Vec<(String, String, String)>>,
        result: Result<(), String>,
    }

    impl FakeSpawner {
        fn new(result: Result<(), String>) -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                result,
            }
        }
    }

    #[tonic::async_trait]
    impl DiscoverySpawner for FakeSpawner {
        async fn spawn(
            &self,
            toolset_name: &str,
            image: &str,
            workspace: &str,
        ) -> Result<(), String> {
            self.calls.lock().unwrap().push((
                toolset_name.to_string(),
                image.to_string(),
                workspace.to_string(),
            ));
            self.result.clone()
        }
    }

    fn seam_state() -> Arc<ControllerState> {
        ControllerState::new(
            None,
            "ns".into(),
            "http://controller:9090".into(),
            SchedulingConfig::default(),
        )
    }

    fn toolset_with_image(image: Option<&str>) -> ToolsetEntry {
        ToolsetEntry {
            image: image.map(String::from),
            keepalive: false,
            profiles: HashMap::new(),
        }
    }

    fn bindings_for(ws: &str, toolsets: &[&str]) -> WorkspaceBindings {
        let mut m = HashMap::new();
        m.insert(
            ws.to_string(),
            toolsets.iter().map(|t| t.to_string()).collect(),
        );
        WorkspaceBindings::from_map(m)
    }

    #[test]
    fn plan_discovery_branches() {
        assert_eq!(plan_discovery(None, &[]), DiscoveryPlan::NoImage);
        assert_eq!(plan_discovery(Some("img"), &[]), DiscoveryPlan::NoWorkspace);
        assert_eq!(
            plan_discovery(Some("img"), &["ws".to_string()]),
            DiscoveryPlan::Spawn {
                workspace: "ws".into()
            }
        );
    }

    #[tokio::test]
    async fn reconcile_toolsets_spawns_one_job_per_configured_toolset() {
        let state = seam_state();
        let spawner = FakeSpawner::new(Ok(()));
        let bindings = bindings_for("ws", &["stdlib", "notion"]);

        let mut entries = HashMap::new();
        entries.insert(
            "stdlib".to_string(),
            toolset_with_image(Some("ghcr.io/test/stdlib:latest")),
        );
        entries.insert(
            "notion".to_string(),
            toolset_with_image(Some("ghcr.io/test/notion:latest")),
        );
        let toolsets = ToolsetConfig::from_map(entries);

        reconcile_toolsets(&state, &spawner, &bindings, &toolsets).await;

        let mut calls = spawner.calls.lock().unwrap().clone();
        calls.sort();
        assert_eq!(
            calls,
            vec![
                (
                    "notion".to_string(),
                    "ghcr.io/test/notion:latest".to_string(),
                    "ws".to_string()
                ),
                (
                    "stdlib".to_string(),
                    "ghcr.io/test/stdlib:latest".to_string(),
                    "ws".to_string()
                ),
            ],
            "every configured toolset gets exactly one discovery spawn"
        );
    }

    #[tokio::test]
    async fn reconcile_spawns_discovery_job_without_in_process_pull() {
        let state = seam_state();
        let spawner = FakeSpawner::new(Ok(()));
        let bindings = bindings_for("ws", &["stdlib"]);
        let ts = toolset_with_image(Some("ghcr.io/test/stdlib:latest"));

        reconcile_toolset(&state, &spawner, &bindings, &ts, "stdlib")
            .await
            .expect("a spawnable toolset reconciles Ok");

        let calls = spawner.calls.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "reconcile must spawn exactly one discovery Job"
        );
        assert_eq!(
            calls[0],
            (
                "stdlib".to_string(),
                "ghcr.io/test/stdlib:latest".to_string(),
                "ws".to_string()
            )
        );
        // No tools registered synchronously: they arrive over the report RPC,
        // never from an in-process registry pull by the controller.
        assert!(
            state.get_tool("Search").await.is_none(),
            "reconcile must not register tools in-process; the report handler does"
        );
        // The toolset spec IS registered so begin_tool_call can resolve its
        // egress once the reported tools land.
        assert!(state.get_toolset("stdlib").await.is_some());
    }

    #[tokio::test]
    async fn reconcile_failed_spawn_leaves_unregistered_and_errors() {
        let state = seam_state();
        let spawner = FakeSpawner::new(Err("k8s API rejected the Job".into()));
        let bindings = bindings_for("ws", &["stdlib"]);
        let ts = toolset_with_image(Some("ghcr.io/test/stdlib:latest"));

        let res = reconcile_toolset(&state, &spawner, &bindings, &ts, "stdlib").await;
        assert!(
            res.is_err(),
            "a failed discovery spawn must return Err so the caller marks the toolset unready"
        );
        assert!(
            state.get_tool("Search").await.is_none(),
            "a failed discovery must leave the toolset's tools unregistered"
        );
        assert!(
            state.get_toolset("stdlib").await.is_none(),
            "a failed discovery must not register the toolset spec (matches today's terminal path)"
        );
    }

    #[tokio::test]
    async fn reconcile_no_image_registers_spec_without_spawning() {
        let state = seam_state();
        let spawner = FakeSpawner::new(Ok(()));
        let bindings = bindings_for("ws", &["prompt-anthropic"]);
        let ts = toolset_with_image(None);

        reconcile_toolset(&state, &spawner, &bindings, &ts, "prompt-anthropic")
            .await
            .expect("an imageless toolset reconciles Ok");

        assert!(
            spawner.calls.lock().unwrap().is_empty(),
            "an imageless toolset must spawn no discovery Job"
        );
        assert!(state.get_toolset("prompt-anthropic").await.is_some());
    }
}
