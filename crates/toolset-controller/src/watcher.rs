use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::{Event, EventSource, ObjectReference};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::PostParams;
use kube::runtime::watcher::{self, Event as WatcherEvent};
use kube::{Api, Client};
use tracing::{error, info, warn};

use crate::crd::{Model, Provider, Toolset};
use crate::job::{build_discovery_job, create_job};
use crate::registry::{DiscoveredTool, RegistryError};
use crate::state::{ControllerState, WorkspaceBindings};
use shared::scheduling::SchedulingConfig;

/// Backoff schedule for toolset tool discovery: initial attempt + five backoffs
/// totalling ~15.5 s. Anything unresolved by then is a hard failure that
/// excludes the toolset from controller state.
const DISCOVERY_BACKOFF_MS: &[u64] = &[500, 1000, 2000, 4000, 8000];

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
    let mut last_err: Option<RegistryError> = None;
    let total = DISCOVERY_BACKOFF_MS.len() + 1;
    for (attempt_idx, delay_ms) in std::iter::once(0_u64)
        .chain(DISCOVERY_BACKOFF_MS.iter().copied())
        .enumerate()
    {
        if attempt_idx > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        match fetch(image.to_string()).await {
            Ok(tools) => return Ok(tools),
            Err(e) if !e.is_retryable() => return Err(e),
            Err(e) => {
                warn!(
                    image,
                    attempt = attempt_idx + 1,
                    total,
                    error = %e,
                    "retryable discovery error"
                );
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        RegistryError::UnexpectedResponse("retry exhausted with no captured error".into())
    }))
}

/// Spawns the ephemeral discovery Job that reads a Toolset image's tool label
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

/// Pure reconcile decision for one Toolset, derived from its image and the
/// workspaces bound to it. No I/O, so the branch selection is unit-testable.
#[derive(Debug, PartialEq)]
pub(crate) enum DiscoveryPlan {
    /// The Toolset declares an image and at least one workspace is bound: spawn
    /// discovery under that workspace's ServiceAccount.
    Spawn { workspace: String },
    /// The Toolset declares no image: it serves no tools.
    NoImage,
    /// The Toolset declares an image but no workspace is bound, so no pod can
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

/// Reconcile one Toolset: decide, then spawn the discovery Job if warranted.
/// The controller performs NO in-process registry pull — discovery reach lives
/// in the spawned Job, which reports its tools back over the report RPC.
///
/// On `Ok` the toolset spec is registered so `begin_tool_call` can resolve its
/// egress once the reported tools land. On spawn failure the toolset's tools
/// are left unregistered and `Err` is returned so the caller marks it unready.
pub(crate) async fn reconcile_toolset(
    state: &ControllerState,
    spawner: &dyn DiscoverySpawner,
    bindings: &WorkspaceBindings,
    toolset: &Toolset,
    name: &str,
) -> Result<(), String> {
    let workspaces = bindings.workspaces_for_toolset(name);
    match plan_discovery(toolset.spec.image.as_deref(), &workspaces) {
        DiscoveryPlan::NoImage => {
            state.remove_tools_for_toolset(name).await;
            state.set_toolset(name.to_string(), toolset.clone()).await;
            Ok(())
        }
        DiscoveryPlan::NoWorkspace => {
            info!(toolset = %name, "toolset has an image but no bound workspace; skipping discovery");
            state.set_toolset(name.to_string(), toolset.clone()).await;
            Ok(())
        }
        DiscoveryPlan::Spawn { workspace } => {
            let image = toolset
                .spec
                .image
                .as_deref()
                .expect("Spawn plan implies an image");
            match spawner.spawn(name, image, &workspace).await {
                Ok(()) => {
                    info!(toolset = %name, %image, %workspace, "spawned discovery Job");
                    state.set_toolset(name.to_string(), toolset.clone()).await;
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

/// Best-effort `Warning` Event on the Toolset CR, surfaced via
/// `kubectl describe toolset <name>` for operators investigating NotReady.
async fn emit_failure_event(client: &Client, namespace: &str, toolset_name: &str, message: &str) {
    let event = Event {
        metadata: ObjectMeta {
            generate_name: Some(format!("{toolset_name}.tool-discovery.")),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        involved_object: ObjectReference {
            api_version: Some("sycophant.md/v1".into()),
            kind: Some("Toolset".into()),
            name: Some(toolset_name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        reason: Some("ToolDiscoveryFailed".into()),
        message: Some(message.to_string()),
        type_: Some("Warning".into()),
        source: Some(EventSource {
            component: Some("toolset-controller".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let api: Api<Event> = Api::namespaced(client.clone(), namespace);
    if let Err(e) = api.create(&PostParams::default(), &event).await {
        warn!(toolset = %toolset_name, error = %e, "failed to emit ToolDiscoveryFailed event");
    }
}

pub async fn watch_toolsets(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    spawner: Arc<dyn DiscoverySpawner>,
    bindings: WorkspaceBindings,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> anyhow::Result<()> {
    let api: Api<Toolset> = Api::namespaced(client.clone(), namespace);
    let watcher_config = watcher::Config::default();
    let mut stream = watcher::watcher(api, watcher_config).boxed();

    let mut failed: HashSet<String> = HashSet::new();
    let mut init_pending: Vec<Toolset> = Vec::new();

    while let Some(event) = stream.try_next().await? {
        match event {
            WatcherEvent::Apply(toolset) => {
                let name = toolset.metadata.name.clone().unwrap_or_default();
                info!(toolset = %name, "toolset applied");
                apply_toolset(
                    &state,
                    spawner.as_ref(),
                    &bindings,
                    &client,
                    namespace,
                    &toolset,
                    &name,
                    &mut failed,
                )
                .await;
                let _ = ready_tx.send(failed.is_empty());
            }
            WatcherEvent::Delete(toolset) => {
                let name = toolset.metadata.name.clone().unwrap_or_default();
                info!(toolset = %name, "toolset deleted");
                state.remove_tools_for_toolset(&name).await;
                state.remove_toolset(&name).await;
                failed.remove(&name);
                let _ = ready_tx.send(failed.is_empty());
            }
            WatcherEvent::Init => {
                info!("toolset watcher initialized, clearing registries");
                state.clear_toolsets().await;
                state.clear_tools().await;
                failed.clear();
                init_pending.clear();
                let _ = ready_tx.send(false);
            }
            WatcherEvent::InitApply(toolset) => {
                init_pending.push(toolset);
            }
            WatcherEvent::InitDone => {
                let pending = std::mem::take(&mut init_pending);
                for toolset in pending {
                    let name = toolset.metadata.name.clone().unwrap_or_default();
                    apply_toolset(
                        &state,
                        spawner.as_ref(),
                        &bindings,
                        &client,
                        namespace,
                        &toolset,
                        &name,
                        &mut failed,
                    )
                    .await;
                }
                let toolset_count = state.toolset_count().await;
                let tool_count = state.tool_count().await;
                let failed_count = failed.len();
                info!(
                    toolset_count,
                    tool_count, failed_count, "toolset watcher initial sync complete"
                );
                let _ = ready_tx.send(failed.is_empty());
            }
        }
    }

    warn!("toolset watcher stream ended");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn apply_toolset(
    state: &ControllerState,
    spawner: &dyn DiscoverySpawner,
    bindings: &WorkspaceBindings,
    client: &Client,
    namespace: &str,
    toolset: &Toolset,
    name: &str,
    failed: &mut HashSet<String>,
) {
    match reconcile_toolset(state, spawner, bindings, toolset, name).await {
        Ok(()) => {
            failed.remove(name);
        }
        Err(err_msg) => {
            emit_failure_event(client, namespace, name, &err_msg).await;
            failed.insert(name.to_string());
        }
    }
}

pub async fn watch_models(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> Result<(), String> {
    let api: Api<Model> = Api::namespaced(client, namespace);
    let mut stream = watcher::watcher(api, watcher::Config::default()).boxed();

    while let Some(event) = stream
        .try_next()
        .await
        .map_err(|e| format!("watcher error: {e}"))?
    {
        match event {
            WatcherEvent::Apply(model) => {
                let name = model.metadata.name.clone().unwrap_or_default();
                info!(model = %name, "model applied");
                state.set_model_spec(name, model.spec).await;
            }
            WatcherEvent::Delete(model) => {
                let name = model.metadata.name.clone().unwrap_or_default();
                info!(model = %name, "model deleted");
                state.remove_model(&name).await;
            }
            WatcherEvent::Init => {
                info!("model watcher initialized");
                state.clear_models().await;
            }
            WatcherEvent::InitApply(model) => {
                let name = model.metadata.name.clone().unwrap_or_default();
                info!(model = %name, "model discovered");
                state.set_model_spec(name, model.spec).await;
            }
            WatcherEvent::InitDone => {
                info!("model watcher initial sync complete");
                let _ = ready_tx.send(true);
            }
        }
    }

    warn!("model watcher stream ended");
    Ok(())
}

pub async fn watch_providers(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> Result<(), String> {
    let api: Api<Provider> = Api::namespaced(client, namespace);
    let mut stream = watcher::watcher(api, watcher::Config::default()).boxed();

    while let Some(event) = stream
        .try_next()
        .await
        .map_err(|e| format!("provider watcher error: {e}"))?
    {
        match event {
            WatcherEvent::Apply(provider) => {
                let name = provider.metadata.name.clone().unwrap_or_default();
                info!(provider = %name, "provider applied");
                state.set_provider_spec(name, provider.spec).await;
            }
            WatcherEvent::Delete(provider) => {
                let name = provider.metadata.name.clone().unwrap_or_default();
                info!(provider = %name, "provider deleted");
                state.remove_provider(&name).await;
            }
            WatcherEvent::Init => {
                info!("provider watcher initialized");
                state.clear_providers().await;
            }
            WatcherEvent::InitApply(provider) => {
                let name = provider.metadata.name.clone().unwrap_or_default();
                info!(provider = %name, "provider discovered");
                state.set_provider_spec(name, provider.spec).await;
            }
            WatcherEvent::InitDone => {
                info!("provider watcher initial sync complete");
                let _ = ready_tx.send(true);
            }
        }
    }

    warn!("provider watcher stream ended");
    Ok(())
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

    // ---- Reconcile seam (discovery-Job spawn) ----

    use crate::crd::ToolsetSpec;
    use std::collections::HashMap;

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
            String::new(),
            String::new(),
            "img".into(),
            SchedulingConfig::default(),
        )
    }

    fn toolset_with_image(name: &str, image: Option<&str>) -> Toolset {
        Toolset::new(
            name,
            ToolsetSpec {
                image: image.map(String::from),
                credentials: vec![],
                egress: vec![],
                keepalive: false,
            },
        )
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
    async fn reconcile_spawns_discovery_job_without_in_process_pull() {
        let state = seam_state();
        let spawner = FakeSpawner::new(Ok(()));
        let bindings = bindings_for("ws", &["stdlib"]);
        let ts = toolset_with_image("stdlib", Some("ghcr.io/test/stdlib:latest"));

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
        let ts = toolset_with_image("stdlib", Some("ghcr.io/test/stdlib:latest"));

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
        let ts = toolset_with_image("prompt-anthropic", None);

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
