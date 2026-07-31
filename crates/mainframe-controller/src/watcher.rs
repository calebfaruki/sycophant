use std::sync::Arc;

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

/// True when the most recently observed generation for a watched
/// resource matches the incoming Apply event's generation, signaling a
/// status-patch echo rather than a spec change. The watch loop
/// short-circuits in that case to skip a redundant apply log.
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
