use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt, TryStreamExt};
use kube::runtime::watcher::{self, Event};
use kube::{Api, Client};
use tracing::{info, warn};

use crate::crd::Mainframe;
use crate::state::ControllerState;

pub async fn watch_mainframes(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> anyhow::Result<()> {
    let api: Api<Mainframe> = Api::namespaced(client.clone(), namespace);
    let watcher_config = watcher::Config::default();
    let mut stream = watcher::watcher(api, watcher_config).boxed();

    while let Some(event) = stream.try_next().await? {
        match event {
            Event::Apply(mf) => {
                let name = mf.metadata.name.clone().unwrap_or_default();
                let generation = mf.metadata.generation.unwrap_or(0);
                if should_dedup_apply(state.last_generation(&name).await, generation) {
                    state.set_mainframe(name.clone(), mf.clone()).await;
                    continue;
                }
                info!(mainframe = %name, generation, "mainframe applied");
                state.set_mainframe(name.clone(), mf.clone()).await;
                state.record_generation(&name, generation).await;
                reconcile_one(&client, namespace, &state, &name, &mf).await;
            }
            Event::Delete(mf) => {
                let name = mf.metadata.name.clone().unwrap_or_default();
                info!(mainframe = %name, "mainframe deleted");
                state.remove_mainframe(&name).await;
            }
            Event::Init => {
                info!("mainframe watcher initialized");
                state.clear().await;
            }
            Event::InitApply(mf) => {
                let name = mf.metadata.name.clone().unwrap_or_default();
                let generation = mf.metadata.generation.unwrap_or(0);
                state.set_mainframe(name.clone(), mf.clone()).await;
                state.record_generation(&name, generation).await;
                reconcile_one(&client, namespace, &state, &name, &mf).await;
            }
            Event::InitDone => {
                let count = state.count().await;
                info!(
                    mainframe_count = count,
                    "mainframe watcher initial sync complete"
                );
                let _ = ready_tx.send(true);
            }
        }
    }

    warn!("mainframe watcher stream ended");
    Ok(())
}

/// Periodic reconcile loop. Sleeps between ticks; each tick re-reconciles every
/// known Mainframe. v0 reconciliation is a no-op for HostPath; the loop exists
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
            if let Some(mf) = state.get_mainframe(&name).await {
                reconcile_one(&client, &namespace, &state, &name, &mf).await;
            }
        }
    }
}

/// Reconcile a single Mainframe. Dispatches by `spec.source.kind`; v0 ships
/// only `HostPath` (no controller work — kubelet mounts the host directory
/// directly into the workspace pod). Unknown kinds log a warning and are
/// otherwise ignored, leaving the Mainframe CR untouched.
async fn reconcile_one(
    _client: &Client,
    _namespace: &str,
    _state: &Arc<ControllerState>,
    name: &str,
    mf: &Mainframe,
) {
    let kind = mf.spec.source.kind.as_str();
    match kind {
        "HostPath" => {
            info!(mainframe = %name, kind, "reconcile no-op for HostPath");
        }
        other => {
            warn!(
                mainframe = %name,
                kind = other,
                "unknown source kind; install the matching adapter or fix the spec"
            );
        }
    }
}

/// True when the most recently observed generation for a mainframe matches
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
