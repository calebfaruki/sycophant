use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt, TryStreamExt};
use kube::runtime::watcher::{self, Event};
use kube::{Api, Client};
use tracing::{info, warn};

use crate::crd::Source;
use crate::state::ControllerState;

pub async fn watch_sources(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> anyhow::Result<()> {
    let api: Api<Source> = Api::namespaced(client.clone(), namespace);
    let watcher_config = watcher::Config::default();
    let mut stream = watcher::watcher(api, watcher_config).boxed();

    while let Some(event) = stream.try_next().await? {
        match event {
            Event::Apply(src) => {
                let name = src.metadata.name.clone().unwrap_or_default();
                let generation = src.metadata.generation.unwrap_or(0);
                if should_dedup_apply(state.last_generation(&name).await, generation) {
                    state.set_source(name.clone(), src.clone()).await;
                    continue;
                }
                info!(source = %name, generation, "source applied");
                state.set_source(name.clone(), src.clone()).await;
                state.record_generation(&name, generation).await;
                reconcile_one(&client, namespace, &state, &name, &src).await;
            }
            Event::Delete(src) => {
                let name = src.metadata.name.clone().unwrap_or_default();
                info!(source = %name, "source deleted");
                state.remove_source(&name).await;
            }
            Event::Init => {
                info!("source watcher initialized");
                state.clear().await;
            }
            Event::InitApply(src) => {
                let name = src.metadata.name.clone().unwrap_or_default();
                let generation = src.metadata.generation.unwrap_or(0);
                state.set_source(name.clone(), src.clone()).await;
                state.record_generation(&name, generation).await;
                reconcile_one(&client, namespace, &state, &name, &src).await;
            }
            Event::InitDone => {
                let count = state.count().await;
                info!(source_count = count, "source watcher initial sync complete");
                let _ = ready_tx.send(true);
            }
        }
    }

    warn!("source watcher stream ended");
    Ok(())
}

/// Periodic reconcile loop. Sleeps between ticks; each tick re-reconciles every
/// known Source. v0 reconciliation is a no-op for HostPath; the loop exists
/// as scaffolding for non-HostPath kinds that arrive later (per ADR 010).
pub async fn refresh_loop(
    client: Client,
    namespace: String,
    state: Arc<ControllerState>,
    loop_sleep_seconds: u64,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(loop_sleep_seconds)).await;
        let names = state.list_names().await;
        for name in names {
            if let Some(src) = state.get_source(&name).await {
                reconcile_one(&client, &namespace, &state, &name, &src).await;
            }
        }
    }
}

/// Reconcile a single Source. Dispatches by `spec.kind`. `HostPath` is a
/// no-op (kubelet mounts the host directory directly). `S3` is a no-op for
/// the controller too — the workspace pod's init container does the
/// actual `aws s3 sync` from the spec; the controller only logs and could
/// be extended later to set status conditions reflecting connectivity
/// probes (out of scope for the initial S3 ship).
async fn reconcile_one(
    _client: &Client,
    _namespace: &str,
    _state: &Arc<ControllerState>,
    name: &str,
    src: &Source,
) {
    let kind = src.spec.kind.as_str();
    match kind {
        "HostPath" => {
            info!(source = %name, kind, "reconcile no-op for HostPath");
        }
        "S3" => {
            info!(source = %name, kind, "reconcile no-op for S3 (init container syncs)");
        }
        other => {
            warn!(
                source = %name,
                kind = other,
                "unknown source kind; install the matching adapter or fix the spec"
            );
        }
    }
}

/// True when the most recently observed generation for a source matches
/// the incoming Apply event's generation, signaling a status-patch echo
/// rather than a spec change. The watch loop short-circuits in that case
/// to avoid re-reconciling work that hasn't conceptually moved.
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
