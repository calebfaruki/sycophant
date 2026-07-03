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

/// Fail any tool calls parked on `tool_name`. Dropping each drained
/// `ToolResultGuard` fires its terminal error `ToolCallResult`, so a
/// `call_tool` awaiting `result_rx` for a chamber we just tore down
/// unblocks instead of hanging to the client deadline. No-op when no call
/// is parked.
async fn fail_pending_calls(state: &ControllerState, tool_name: &str) {
    drop(state.take_result_txs_for_tool(tool_name).await);
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
                    fail_pending_calls(state, tool_name).await;
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
            None => {
                state.remove_active_job(tool_name).await;
                fail_pending_calls(state, tool_name).await;
            }
        }
    }

    if !expired.is_empty() {
        warn!(count = expired.len(), "cleaned up expired keepalive Jobs");
    }
}

/// React to a chamber tool-Job lifecycle event. On a terminal
/// (`job_is_terminal`) or deleted Job, drop the in-memory active-job entry
/// and fail any `call_tool` still parked on that tool — the reactive
/// complement to `remove_expired_jobs`, catching a crashed or
/// externally-deleted chamber in seconds instead of at the idle window.
/// Idempotent with the idle sweep. Returns true if it acted. Exposed for
/// tests (drive with a synthetic Job — no apiserver).
pub async fn handle_job_event(state: &ControllerState, job: &Job, deleted: bool) -> bool {
    if !deleted && !shared::keepalive::job_is_terminal(job) {
        return false;
    }
    let Some(tool) = job
        .metadata
        .labels
        .as_ref()
        .and_then(|l| l.get("sycophant.md/tool"))
        .cloned()
    else {
        return false;
    };
    state.remove_active_job(&tool).await;
    fail_pending_calls(state, &tool).await;
    true
}

/// Watch chamber tool Jobs and react to terminal/deleted Jobs via
/// `handle_job_event`. Selector mirrors `reconcile_active_jobs`
/// (`app.kubernetes.io/part-of=sycophant`); non-tool Jobs are ignored by
/// the handler. Uses the existing batch/jobs:watch grant — no new RBAC.
/// Returns `Err` on a stream error so `spawn_watcher_task` restarts it.
pub async fn watch_tool_jobs(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
) -> Result<(), String> {
    shared::keepalive::watch_jobs(
        client,
        namespace,
        "app.kubernetes.io/part-of=sycophant",
        "tool",
        {
            move |job, deleted| {
                let state = state.clone();
                async move {
                    handle_job_event(&state, &job, deleted).await;
                }
            }
        },
    )
    .await
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

    #[tokio::test]
    async fn remove_expired_jobs_fails_parked_call() {
        // RED before the fix: the reap removes the ActiveJob but leaves the
        // result sender in result_txs, so call_tool's parked result_rx
        // never resolves and the call hangs to the client deadline. GREEN
        // after: the reap drains + drops the guard, which emits a terminal
        // error result.
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        let (tx, rx) = tokio::sync::oneshot::channel::<crate::state::ToolCallResult>();
        state
            .set_result_tx("call-1".into(), "test-tool".into(), tx)
            .await;
        let (name, job) = make_active_job("test-tool", 120, 60);
        state.set_active_job(name, job).await;

        let expired = find_expired_jobs(&state, Instant::now()).await;
        remove_expired_jobs(&state, &expired).await;

        let result = tokio::time::timeout(Duration::from_millis(100), rx)
            .await
            .expect("parked call must not hang after reap")
            .expect("guard must deliver a terminal result");
        assert!(result.is_error, "reaped call must terminate as an error");
    }

    #[tokio::test]
    async fn tool_result_guard_drop_emits_error() {
        // Mutant: drop the send in ToolResultGuard::Drop → the receiver
        // observes RecvError (channel closed) and this expect fails.
        let (tx, rx) = tokio::sync::oneshot::channel::<crate::state::ToolCallResult>();
        let guard = crate::state::ToolResultGuard::new(tx);
        drop(guard);
        let result = rx.await.expect("guard Drop must deliver a result");
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn tool_result_guard_send_is_single_delivery() {
        // send() consumes the sender; Drop afterward must be a no-op, so
        // the receiver sees exactly the success result.
        let (tx, rx) = tokio::sync::oneshot::channel::<crate::state::ToolCallResult>();
        let guard = crate::state::ToolResultGuard::new(tx);
        let _ = guard.send(crate::state::ToolCallResult {
            output: "ok".into(),
            is_error: false,
            exit_code: 0,
        });
        let result = rx.await.expect("send must deliver");
        assert!(!result.is_error);
        assert_eq!(result.output, "ok");
    }

    fn tool_job(tool: &str, status: k8s_openapi::api::batch::v1::JobStatus) -> Job {
        use std::collections::BTreeMap;
        let mut job = Job::default();
        let mut labels = BTreeMap::new();
        labels.insert(
            "app.kubernetes.io/part-of".to_string(),
            "sycophant".to_string(),
        );
        labels.insert("sycophant.md/tool".to_string(), tool.to_string());
        job.metadata.labels = Some(labels);
        job.status = Some(status);
        job
    }

    #[tokio::test]
    async fn handle_job_event_fails_parked_call_on_terminal() {
        // Reactive complement to the idle sweep: a Failed chamber Job fails
        // the parked call + drops the active-job entry immediately. Mutant:
        // skip fail_pending_calls → the parked result_rx hangs (timeout).
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        let (tx, rx) = tokio::sync::oneshot::channel::<crate::state::ToolCallResult>();
        state
            .set_result_tx("call-1".into(), "tool-x".into(), tx)
            .await;
        let (name, job) = make_active_job("tool-x", 0, 60);
        state.set_active_job(name, job).await;

        let j = tool_job(
            "tool-x",
            JobStatus {
                failed: Some(1),
                ..Default::default()
            },
        );
        assert!(
            handle_job_event(&state, &j, false).await,
            "must act on terminal"
        );

        let result = tokio::time::timeout(Duration::from_millis(100), rx)
            .await
            .expect("must not hang")
            .expect("guard delivers terminal");
        assert!(result.is_error);
        assert_eq!(state.active_job_count().await, 0);
    }

    #[tokio::test]
    async fn handle_job_event_ignores_nonterminal_apply() {
        // A still-running chamber Job must not fail the call. Mutant: treat
        // every Apply as terminal → live tool calls get killed.
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        );
        let j = tool_job(
            "tool-x",
            JobStatus {
                active: Some(1),
                ..Default::default()
            },
        );
        assert!(!handle_job_event(&state, &j, false).await);
    }
}
