use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::{future, StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::{Event, EventSource, ObjectReference};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::PostParams;
use kube::runtime::watcher::{self, Event as WatcherEvent};
use kube::{Api, Client};
use tracing::{error, info, warn};

use crate::crd::Chamber;
use crate::registry::{self, DiscoveredTool, RegistryError};
use crate::state::{ControllerState, RegisteredTool};

/// Backoff schedule for chamber tool discovery: initial attempt + five backoffs
/// totalling ~15.5 s. Anything that hasn't resolved by then is treated as a
/// hard failure that excludes the chamber from controller state.
const DISCOVERY_BACKOFF_MS: &[u64] = &[500, 1000, 2000, 4000, 8000];

/// Run `fetch` against `image` with bounded retry. Retries `RegistryError`s
/// where `is_retryable()` is true; deterministic errors (`InvalidLabel`,
/// `InvalidImageRef`) bail on the first attempt.
pub(crate) async fn retry_discovery<F, Fut>(
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

/// Discover tools and register them in state. Returns `Ok(count)` on success.
/// On failure: clears any prior tools for this chamber, returns `Err`. The
/// caller decides whether to register the chamber CR in state.
async fn discover_and_register_tools(
    state: &ControllerState,
    chamber_name: &str,
    image: &str,
) -> Result<usize, String> {
    let outcome =
        retry_discovery(image, |i| async move { registry::discover_tools(&i).await }).await;
    match outcome {
        Ok(discovered) => {
            let tools: Vec<RegisteredTool> = discovered
                .into_iter()
                .map(|d| RegisteredTool {
                    name: d.name.clone(),
                    chamber_name: chamber_name.to_string(),
                    description: d
                        .description
                        .unwrap_or_else(|| format!("Invokes the {} tool.", d.name)),
                    image: image.to_string(),
                    args: d.args,
                })
                .collect();
            let count = tools.len();
            state.set_tools_for_chamber(chamber_name, tools).await;
            info!(chamber = %chamber_name, %image, count, "discovered tools from image");
            Ok(count)
        }
        Err(e) => {
            error!(chamber = %chamber_name, %image, error = %e, "tool discovery failed after retries");
            state.remove_tools_for_chamber(chamber_name).await;
            Err(e.to_string())
        }
    }
}

/// Best-effort `Warning` Event on the Chamber CR. Surface via
/// `kubectl describe chamber <name>` for operators investigating NotReady.
async fn emit_failure_event(client: &Client, namespace: &str, chamber_name: &str, message: &str) {
    let event = Event {
        metadata: ObjectMeta {
            generate_name: Some(format!("{chamber_name}.tool-discovery.")),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        involved_object: ObjectReference {
            api_version: Some("sycophant.md/v1".into()),
            kind: Some("Chamber".into()),
            name: Some(chamber_name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        reason: Some("ToolDiscoveryFailed".into()),
        message: Some(message.to_string()),
        type_: Some("Warning".into()),
        source: Some(EventSource {
            component: Some("airlock-controller".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let api: Api<Event> = Api::namespaced(client.clone(), namespace);
    if let Err(e) = api.create(&PostParams::default(), &event).await {
        warn!(chamber = %chamber_name, error = %e, "failed to emit ToolDiscoveryFailed event");
    }
}

pub async fn watch_chambers(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> anyhow::Result<()> {
    let api: Api<Chamber> = Api::namespaced(client.clone(), namespace);
    let watcher_config = watcher::Config::default();
    let mut stream = watcher::watcher(api, watcher_config).boxed();

    let mut failed: HashSet<String> = HashSet::new();
    let mut init_pending: Vec<Chamber> = Vec::new();

    while let Some(event) = stream.try_next().await? {
        match event {
            WatcherEvent::Apply(chamber) => {
                let name = chamber.metadata.name.clone().unwrap_or_default();
                info!(chamber = %name, "chamber applied");
                apply_chamber(&state, &client, namespace, &chamber, &name, &mut failed).await;
                let _ = ready_tx.send(failed.is_empty());
            }
            WatcherEvent::Delete(chamber) => {
                let name = chamber.metadata.name.clone().unwrap_or_default();
                info!(chamber = %name, "chamber deleted");
                state.remove_tools_for_chamber(&name).await;
                state.remove_chamber(&name).await;
                failed.remove(&name);
                let _ = ready_tx.send(failed.is_empty());
            }
            WatcherEvent::Init => {
                info!("chamber watcher initialized, clearing registries");
                state.clear_chambers().await;
                state.clear_tools().await;
                failed.clear();
                init_pending.clear();
                // Don't claim readiness during resync.
                let _ = ready_tx.send(false);
            }
            WatcherEvent::InitApply(chamber) => {
                init_pending.push(chamber);
            }
            WatcherEvent::InitDone => {
                let pending = std::mem::take(&mut init_pending);
                let outcomes = future::join_all(pending.into_iter().map(|chamber| {
                    let state = state.clone();
                    let client = client.clone();
                    let namespace = namespace.to_string();
                    async move {
                        let name = chamber.metadata.name.clone().unwrap_or_default();
                        let outcome = match &chamber.spec.image {
                            Some(image) => {
                                let res = discover_and_register_tools(&state, &name, image).await;
                                if let Err(ref err_msg) = res {
                                    emit_failure_event(&client, &namespace, &name, err_msg).await;
                                }
                                res
                            }
                            None => {
                                state.remove_tools_for_chamber(&name).await;
                                Ok(0)
                            }
                        };
                        (name, chamber, outcome)
                    }
                }))
                .await;

                for (name, chamber, outcome) in outcomes {
                    match outcome {
                        Ok(_) => {
                            state.set_chamber(name.clone(), chamber).await;
                            failed.remove(&name);
                        }
                        Err(_) => {
                            failed.insert(name);
                        }
                    }
                }
                let chamber_count = state.chamber_count().await;
                let tool_count = state.tool_count().await;
                let failed_count = failed.len();
                info!(
                    chamber_count,
                    tool_count, failed_count, "chamber watcher initial sync complete"
                );
                let _ = ready_tx.send(failed.is_empty());
            }
        }
    }

    warn!("chamber watcher stream ended");
    Ok(())
}

async fn apply_chamber(
    state: &ControllerState,
    client: &Client,
    namespace: &str,
    chamber: &Chamber,
    name: &str,
    failed: &mut HashSet<String>,
) {
    let outcome = match &chamber.spec.image {
        Some(image) => {
            let res = discover_and_register_tools(state, name, image).await;
            if let Err(ref err_msg) = res {
                emit_failure_event(client, namespace, name, err_msg).await;
            }
            res
        }
        None => {
            state.remove_tools_for_chamber(name).await;
            Ok(0)
        }
    };
    match outcome {
        Ok(_) => {
            state.set_chamber(name.to_string(), chamber.clone()).await;
            failed.remove(name);
        }
        Err(_) => {
            failed.insert(name.to_string());
        }
    }
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
}
