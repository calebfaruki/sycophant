use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::api::batch::v1::Job;
use kube::api::ListParams;
use kube::{Api, Client};
use tracing::{error, info, warn};

use crate::state::{ActiveJob, ControllerState};
use shared::keepalive::delete_job;

const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

/// Idle window before a keepalive Job is reaped. Bumped by
/// `state::bump_last_activity` on every `send_tool_result`, so a busy
/// pod with continuous traffic never expires. Per-chamber durations
/// (replacing `keepalive: bool` with `keepalive_seconds: Option<u64>`)
/// is a deferred CRD-breaking change.
pub const KEEPALIVE_IDLE_SECONDS: u64 = 600;

pub async fn find_expired_jobs(state: &ControllerState, now: Instant) -> Vec<(String, String)> {
    state
        .list_active_jobs()
        .await
        .into_iter()
        .filter(|(_, _, keepalive_secs, last_activity)| {
            *keepalive_secs > 0 && now.duration_since(*last_activity).as_secs() >= *keepalive_secs
        })
        .map(|(name, job_name, _, _)| (name, job_name))
        .collect()
}

/// Delete each expired Job from the kube API, then drop the matching
/// state entry. Order is **k8s first, then state**: the inverse leaves
/// the controller's view ahead of the cluster ("no job" while a Pod
/// still runs), which `call_tool`'s dedup check would miss → duplicate
/// Job on the next call and an adversarial workspace pod the controller
/// has lost track of. k8s-first failure self-heals on the next 30s
/// tick; 404 collapses to success.
pub async fn remove_expired_jobs(state: &ControllerState, expired: &[(String, String)]) {
    let client = state.kube_client().cloned();
    for (tool_name, job_name) in expired {
        match &client {
            Some(c) => match delete_job(c, state.namespace(), job_name).await {
                Ok(()) => {
                    info!(tool = %tool_name, job = %job_name, "deleted idle keepalive Job");
                    state.remove_active_job(tool_name).await;
                }
                Err(kube::Error::Api(e)) => {
                    error!(
                        code = e.code,
                        tool = %tool_name,
                        job = %job_name,
                        message = %e.message,
                        "kube API rejected delete; retrying next tick"
                    );
                }
                Err(e) => {
                    warn!(tool = %tool_name, job = %job_name, err = %e, "delete transient failure, retrying next tick");
                }
            },
            // Unit-test path: no kube client wired. Preserves existing
            // fixtures that exercise expiry semantics with `ControllerState::new(None, ..)`.
            None => state.remove_active_job(tool_name).await,
        }
    }

    if !expired.is_empty() {
        warn!(count = expired.len(), "cleaned up expired keepalive Jobs");
    }
}

pub async fn cleanup_loop(state: Arc<ControllerState>) {
    loop {
        tokio::time::sleep(CLEANUP_INTERVAL).await;
        let expired = find_expired_jobs(&state, Instant::now()).await;
        remove_expired_jobs(&state, &expired).await;
    }
}

/// Re-adopt existing keepalive Jobs into the in-memory `active_jobs`
/// map on controller startup. Without this, after a controller restart
/// the next `call_tool` would observe `get_active_job=None`, dedup
/// check would create a duplicate Job, and the old Job's pod would
/// poll forever for assignments that never arrive.
///
/// Fires once after the chamber watcher's first sync — needed because
/// `state.get_chamber()` resolves `keepalive_seconds` per-chamber.
pub async fn reconcile_active_jobs(
    client: &Client,
    namespace: &str,
    state: &ControllerState,
) -> Result<(), kube::Error> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels("app.kubernetes.io/part-of=sycophant");
    let list = jobs.list(&lp).await?;
    let mut adopted = 0usize;
    for job in list.items {
        // Skip Jobs that already completed; the cleanup loop or kube
        // TTL controller will dispose of them.
        if job
            .status
            .as_ref()
            .and_then(|s| s.completion_time.as_ref())
            .is_some()
        {
            continue;
        }
        let labels = match job.metadata.labels.as_ref() {
            Some(l) => l,
            None => continue,
        };
        let tool_name = match labels.get("sycophant.md/tool") {
            Some(t) => t.clone(),
            None => continue,
        };
        let chamber_name = match labels.get("sycophant.md/chamber") {
            Some(c) => c.clone(),
            None => continue,
        };
        let job_name = match job.metadata.name.clone() {
            Some(n) => n,
            None => continue,
        };
        let keepalive_seconds = match state.get_chamber(&chamber_name).await {
            Some(c) if c.spec.keepalive => KEEPALIVE_IDLE_SECONDS,
            _ => 0,
        };
        state
            .set_active_job(
                tool_name.clone(),
                ActiveJob {
                    job_name,
                    tool_name,
                    last_activity: Instant::now(),
                    keepalive_seconds,
                },
            )
            .await;
        adopted += 1;
    }
    if adopted > 0 {
        info!(
            count = adopted,
            "reconciled existing keepalive Jobs into active_jobs map"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ActiveJob;

    fn make_active_job(tool: &str, idle_secs: u64, keepalive_secs: u64) -> (String, ActiveJob) {
        (
            tool.to_string(),
            ActiveJob {
                job_name: format!("airlock-{tool}-abc"),
                tool_name: tool.to_string(),
                last_activity: Instant::now() - Duration::from_secs(idle_secs),
                keepalive_seconds: keepalive_secs,
            },
        )
    }

    #[tokio::test]
    async fn expired_job_removed() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        let (name, job) = make_active_job("test-tool", 120, 60);
        state.set_active_job(name, job).await;

        let expired = find_expired_jobs(&state, Instant::now()).await;
        remove_expired_jobs(&state, &expired).await;

        assert_eq!(state.active_job_count().await, 0);
    }

    #[tokio::test]
    async fn active_job_not_removed() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        let (name, job) = make_active_job("active-tool", 0, 300);
        state.set_active_job(name, job).await;

        let expired = find_expired_jobs(&state, Instant::now()).await;
        remove_expired_jobs(&state, &expired).await;

        assert_eq!(state.active_job_count().await, 1);
    }

    #[tokio::test]
    async fn zero_keepalive_never_expires() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        let (name, job) = make_active_job("fire-forget", 9999, 0);
        state.set_active_job(name, job).await;

        let expired = find_expired_jobs(&state, Instant::now()).await;
        assert!(expired.is_empty());
    }

    #[tokio::test]
    async fn multiple_expired_at_once() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        let (n1, j1) = make_active_job("tool-a", 120, 60);
        let (n2, j2) = make_active_job("tool-b", 200, 60);
        state.set_active_job(n1, j1).await;
        state.set_active_job(n2, j2).await;

        let expired = find_expired_jobs(&state, Instant::now()).await;
        assert_eq!(expired.len(), 2);

        remove_expired_jobs(&state, &expired).await;
        assert_eq!(state.active_job_count().await, 0);
    }
}
