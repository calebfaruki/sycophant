use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt, TryStreamExt};
use kube::runtime::watcher::{self, Event};
use kube::{Api, Client};
use tracing::{info, warn};

use crate::crd::Kernel;
use crate::state::ControllerState;

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

/// Periodic reconcile loop. Sleeps between ticks; each tick re-reconciles
/// every known Kernel. `HostPath` kernels are a no-op (kubelet mounts the
/// host directory directly); `S3` kernels have the controller create an
/// idempotent one-shot sync Job.
pub async fn refresh_loop(
    client: Client,
    namespace: String,
    state: Arc<ControllerState>,
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
    }
}

/// Reconcile a single Kernel. Dispatches by `spec.kind`. `HostPath` is a
/// no-op (kubelet mounts the host directory directly). `S3` has the
/// controller create an idempotent one-shot Job that `aws s3 sync`s the
/// spec's source into the workspace's kernel directory on the writer PVC.
async fn reconcile_kernel(
    client: &Client,
    namespace: &str,
    _state: &Arc<ControllerState>,
    name: &str,
    k: &Kernel,
) {
    let kind = k.spec.kind.as_str();
    match kind {
        "HostPath" => {
            info!(kernel = %name, kind, "reconcile no-op for HostPath");
        }
        "S3" => match k.spec.s3.as_ref() {
            Some(s3) => {
                if let Err(e) = crate::job::create_s3_sync_job(
                    client,
                    name,
                    namespace,
                    s3,
                    crate::job::AWS_CLI_IMAGE,
                )
                .await
                {
                    warn!(kernel = %name, error = %e, "failed to create S3 sync Job");
                }
            }
            None => {
                warn!(kernel = %name, "S3 kernel missing spec.s3; skipping sync");
            }
        },
        other => {
            warn!(
                kernel = %name,
                kind = other,
                "unknown kernel kind; install the matching adapter or fix the spec"
            );
        }
    }
}

/// True when the most recently observed generation for a watched
/// resource matches the incoming Apply event's generation, signaling a
/// status-patch echo rather than a spec change. The watch loop
/// short-circuits in that case to avoid re-reconciling work that hasn't
/// conceptually moved.
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
