use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt, TryStreamExt};
use kube::runtime::watcher::{self, Event};
use kube::{Api, Client};
use tracing::{error, info, warn};

use crate::crd::Kernel;
use crate::finalizer::{deletion_requeue_delay, ensure_finalizer, process_deletion, DeletionStep};
use crate::materialize::{materialize_children, MaterializationContext};
use crate::state::ControllerState;
use crate::workspace_crd::Workspace;

pub async fn watch_kernels(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> anyhow::Result<()> {
    let api: Api<Kernel> = Api::namespaced(client.clone(), namespace);
    let watcher_config = watcher::Config::default();
    let mut stream = watcher::watcher(api, watcher_config).boxed();

    while let Some(event) = stream.try_next().await? {
        match event {
            Event::Apply(k) => {
                let name = k.metadata.name.clone().unwrap_or_default();
                let generation = k.metadata.generation.unwrap_or(0);
                if should_dedup_apply(state.last_kernel_generation(&name).await, generation) {
                    state.set_kernel(name.clone(), k.clone()).await;
                    continue;
                }
                info!(kernel = %name, generation, "kernel applied");
                state.set_kernel(name.clone(), k.clone()).await;
                state.record_kernel_generation(&name, generation).await;
                reconcile_kernel(&client, namespace, &state, &name, &k).await;
            }
            Event::Delete(k) => {
                let name = k.metadata.name.clone().unwrap_or_default();
                info!(kernel = %name, "kernel deleted");
                state.remove_kernel(&name).await;
            }
            Event::Init => {
                info!("kernel watcher initialized");
                state.clear_kernels().await;
            }
            Event::InitApply(k) => {
                let name = k.metadata.name.clone().unwrap_or_default();
                let generation = k.metadata.generation.unwrap_or(0);
                state.set_kernel(name.clone(), k.clone()).await;
                state.record_kernel_generation(&name, generation).await;
                reconcile_kernel(&client, namespace, &state, &name, &k).await;
            }
            Event::InitDone => {
                let count = state.kernel_count().await;
                info!(kernel_count = count, "kernel watcher initial sync complete");
                let _ = ready_tx.send(true);
            }
        }
    }

    warn!("kernel watcher stream ended");
    Ok(())
}

/// Watch Workspace CRs. Reconciles each observed Workspace by ensuring
/// the finalizer is set and materializing the child Pod + SA + PVC +
/// NetworkPolicy resources. On deletion, polls until the Pod is
/// confirmed gone before removing the finalizer.
pub async fn watch_workspaces(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    ctx: Arc<MaterializationContext>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> anyhow::Result<()> {
    let api: Api<Workspace> = Api::namespaced(client.clone(), namespace);
    let watcher_config = watcher::Config::default();
    let mut stream = watcher::watcher(api, watcher_config).boxed();

    while let Some(event) = stream.try_next().await? {
        match event {
            Event::Apply(w) => {
                let name = w.metadata.name.clone().unwrap_or_default();
                let generation = w.metadata.generation.unwrap_or(0);
                if should_dedup_apply(state.last_workspace_generation(&name).await, generation) {
                    state.set_workspace(name.clone(), w.clone()).await;
                    continue;
                }
                info!(workspace = %name, generation, "workspace applied");
                state.set_workspace(name.clone(), w.clone()).await;
                state.record_workspace_generation(&name, generation).await;
                reconcile_workspace(&client, namespace, &state, &ctx, &name, &w).await;
            }
            Event::Delete(w) => {
                let name = w.metadata.name.clone().unwrap_or_default();
                info!(workspace = %name, "workspace deleted (K8s confirmed); state cleanup");
                state.remove_workspace(&name).await;
            }
            Event::Init => {
                info!("workspace watcher initialized");
                state.clear_workspaces().await;
            }
            Event::InitApply(w) => {
                let name = w.metadata.name.clone().unwrap_or_default();
                let generation = w.metadata.generation.unwrap_or(0);
                state.set_workspace(name.clone(), w.clone()).await;
                state.record_workspace_generation(&name, generation).await;
                reconcile_workspace(&client, namespace, &state, &ctx, &name, &w).await;
            }
            Event::InitDone => {
                let count = state.workspace_count().await;
                info!(
                    workspace_count = count,
                    "workspace watcher initial sync complete"
                );
                let _ = ready_tx.send(true);
            }
        }
    }

    warn!("workspace watcher stream ended");
    Ok(())
}

/// Periodic reconcile loop. Sleeps between ticks; each tick re-reconciles every
/// known Kernel and Workspace. Kernel reconciliation is a no-op (kubelet
/// handles HostPath mounts; the workspace pod's init container handles
/// S3 sync). Workspace reconciliation re-applies the four child resources
/// idempotently via server-side apply.
pub async fn refresh_loop(
    client: Client,
    namespace: String,
    state: Arc<ControllerState>,
    ctx: Arc<MaterializationContext>,
    loop_sleep_seconds: u64,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(loop_sleep_seconds)).await;
        let kernel_names = state.list_kernel_names().await;
        for name in kernel_names {
            if let Some(k) = state.get_kernel(&name).await {
                reconcile_kernel(&client, &namespace, &state, &name, &k).await;
            }
        }
        let workspace_names = state.list_workspace_names().await;
        for name in workspace_names {
            if let Some(w) = state.get_workspace(&name).await {
                reconcile_workspace(&client, &namespace, &state, &ctx, &name, &w).await;
            }
        }
    }
}

/// Reconcile a single Kernel. Dispatches by `spec.kind`. `HostPath` is a
/// no-op (kubelet mounts the host directory directly). `S3` is a no-op for
/// the controller too — the workspace pod's init container does the
/// actual `aws s3 sync` from the spec.
async fn reconcile_kernel(
    _client: &Client,
    _namespace: &str,
    _state: &Arc<ControllerState>,
    name: &str,
    k: &Kernel,
) {
    let kind = k.spec.kind.as_str();
    match kind {
        "HostPath" => {
            info!(kernel = %name, kind, "reconcile no-op for HostPath");
        }
        "S3" => {
            info!(kernel = %name, kind, "reconcile no-op for S3 (init container syncs)");
        }
        other => {
            warn!(
                kernel = %name,
                kind = other,
                "unknown kernel kind; install the matching adapter or fix the spec"
            );
        }
    }
}

/// Reconcile a single Workspace. Two paths:
///
/// 1. Live workspace (no `deletionTimestamp`): ensure the finalizer is
///    in place, then SSA-apply the four child resources.
/// 2. Pending-delete workspace (`deletionTimestamp` set): loop until
///    the Pod is confirmed gone, then remove the finalizer so K8s can
///    finalize the Workspace deletion. The loop blocks the watcher
///    event-loop briefly; deletions are rare so this is acceptable in
///    exchange for simpler control flow.
async fn reconcile_workspace(
    client: &Client,
    namespace: &str,
    _state: &Arc<ControllerState>,
    ctx: &Arc<MaterializationContext>,
    name: &str,
    workspace: &Workspace,
) {
    if workspace.metadata.deletion_timestamp.is_some() {
        loop {
            match process_deletion(client, namespace, workspace).await {
                Ok(DeletionStep::FinalizerRemoved) => return,
                Ok(DeletionStep::WaitForPod) => {
                    tokio::time::sleep(deletion_requeue_delay()).await;
                }
                Err(e) => {
                    error!(
                        workspace = %name,
                        error = %e,
                        "deletion reconcile errored; will retry on next observe"
                    );
                    return;
                }
            }
        }
    }

    if let Err(e) = ensure_finalizer(client, namespace, workspace).await {
        error!(
            workspace = %name,
            error = %e,
            "failed to ensure finalizer; child materialization skipped this pass"
        );
        return;
    }

    match materialize_children(client, namespace, ctx, workspace).await {
        Ok(()) => {
            info!(
                workspace = %name,
                image = %workspace.spec.image,
                tag = %workspace.spec.tag,
                "workspace children materialized"
            );
        }
        Err(e) => {
            error!(
                workspace = %name,
                error = %e,
                "child materialization failed; will retry on next reconcile"
            );
        }
    }
}

/// True when the most recently observed generation for a watched
/// resource matches the incoming Apply event's generation, signaling a
/// status-patch echo rather than a spec change. The watch loop
/// short-circuits in that case to avoid re-reconciling work that hasn't
/// conceptually moved. Reused across both Kernel and Workspace watchers
/// because the dedup property is identical at the metadata-generation
/// level.
fn should_dedup_apply(last: Option<i64>, current: i64) -> bool {
    last == Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_matches_when_generations_equal() {
        assert!(should_dedup_apply(Some(5), 5));
        assert!(should_dedup_apply(Some(0), 0));
    }

    #[test]
    fn dedup_does_not_match_on_different_generation() {
        assert!(!should_dedup_apply(Some(4), 5));
        assert!(!should_dedup_apply(Some(6), 5));
    }

    #[test]
    fn dedup_does_not_match_when_no_prior_generation() {
        assert!(!should_dedup_apply(None, 5));
        assert!(!should_dedup_apply(None, 0));
    }
}
