//! Idle keepalive for tool Jobs.
//!
//! Per-tool Jobs are tracked in `active_jobs`, with a 30s idle sweep plus a
//! reactive Job watch that fails parked work the instant a tool job's Job goes
//! terminal or is deleted. Delete is k8s-first: clearing state ahead of the
//! cluster would leave the controller's view ahead of reality and let the next
//! dispatch spawn a duplicate Job. K8s-first failure self-heals on the next
//! tick; 404 collapses to success.

use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::api::batch::v1::Job;
use kube::api::ListParams;
use kube::{Api, Client};
use tracing::{error, info, warn};

use crate::state::{ActiveJob, ControllerState, RecordEviction};
use shared::keepalive::delete_job;

const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

// =========================================================================
// Tool-job keepalive
// =========================================================================

/// Idle window before a keepalive tool Job is reaped. Bumped by
/// `bump_last_activity` on every completed `stream_tool_result`, so a busy pod
/// with continuous traffic never expires.
pub const TOOL_KEEPALIVE_IDLE_SECONDS: u64 = 600;

pub async fn find_expired_jobs(
    state: &ControllerState,
    now: Instant,
) -> Vec<(String, String, Option<String>, String)> {
    state
        .list_active_jobs()
        .await
        .into_iter()
        .filter(|(_, _, keepalive_secs, last_activity)| {
            *keepalive_secs > 0 && now.duration_since(*last_activity).as_secs() >= *keepalive_secs
        })
        .map(|((workspace, tool_name, grant), job_name, _, _)| {
            (workspace, tool_name, grant, job_name)
        })
        .collect()
}

/// Delete each expired tool Job from the kube API, then drop the matching
/// state entry (k8s first, then state).
pub async fn remove_expired_jobs(
    state: &ControllerState,
    expired: &[(String, String, Option<String>, String)],
) {
    let client = state.kube_client().cloned();
    for (workspace, tool_name, grant, job_name) in expired {
        match &client {
            Some(c) => match delete_job(c, state.namespace(), job_name).await {
                Ok(()) => {
                    info!(workspace = %workspace, tool = %tool_name, job = %job_name, "deleted idle keepalive Job");
                    // The delete above is an apiserver round trip taken under no
                    // dispatch lock, so the slot may hold a replacement by now.
                    // Only Removed is ours to drain.
                    if state
                        .remove_active_job_named(workspace, tool_name, job_name)
                        .await
                        == RecordEviction::Removed
                    {
                        state.retire_calls_for_tool_job(workspace, tool_name).await;
                    }
                }
                Err(kube::Error::Api(e)) => {
                    error!(
                        code = e.code,
                        workspace = %workspace,
                        tool = %tool_name,
                        job = %job_name,
                        message = %e.message,
                        "kube API rejected delete; retrying next tick"
                    );
                }
                Err(e) => {
                    warn!(workspace = %workspace, tool = %tool_name, job = %job_name, err = %e, "delete transient failure, retrying next tick");
                }
            },
            // Unit-test path: no kube client wired.
            None => {
                state
                    .remove_active_job(workspace, tool_name, grant.as_deref())
                    .await;
                state.retire_calls_for_tool_job(workspace, tool_name).await;
            }
        }
    }

    if !expired.is_empty() {
        warn!(count = expired.len(), "cleaned up expired keepalive Jobs");
    }
}

/// React to a tool Job lifecycle event. On a terminal or deleted Job,
/// drop the in-memory active-job entry and fail any call still parked on that
/// tool. Idempotent with the idle sweep. Returns true if it acted.
pub async fn handle_tool_job_event(state: &ControllerState, job: &Job, deleted: bool) -> bool {
    if !deleted && !shared::keepalive::job_is_terminal(job) {
        return false;
    }
    let labels = match job.metadata.labels.as_ref() {
        Some(l) => l,
        None => return false,
    };
    let Some(tool) = labels.get("sycophant.md/tool").cloned() else {
        return false;
    };
    let Some(workspace) = labels.get("sycophant.md/workspace").cloned() else {
        return false;
    };
    let Some(job_name) = job.metadata.name.as_deref() else {
        return false;
    };
    // A record naming a different Job belongs to a live successor. No record
    // means whichever path removed it already drained this key.
    if state
        .remove_active_job_named(&workspace, &tool, job_name)
        .await
        != RecordEviction::Removed
    {
        return false;
    }
    state.retire_calls_for_tool_job(&workspace, &tool).await;
    true
}

/// Label value `build_discovery_job` stamps on the ephemeral discovery Job.
const DISCOVERY_JOB_LABEL: &str = "discovery";

/// Report a failed discovery Job. Discovery Jobs carry no `sycophant.md/tool`,
/// so `handle_tool_job_event` ignores them and a failure would otherwise be
/// silent: the toolset stays empty and the only symptom is a `NotFound` at
/// tool-call time, arbitrarily later. Log-only, no requeue — the Job already
/// retries the registry read and the report in-process, so a terminal Job means
/// a terminal error and its tools stay unregistered until this controller
/// rolls. Returns true if it acted.
fn handle_discovery_job_event(job: &Job, deleted: bool) -> bool {
    // `ttlSecondsAfterFinished` deletes every discovery Job, healthy ones
    // included, so a Delete carries no failure signal.
    if deleted {
        return false;
    }
    let Some(labels) = job.metadata.labels.as_ref() else {
        return false;
    };
    if labels.get("sycophant.md/job").map(String::as_str) != Some(DISCOVERY_JOB_LABEL) {
        return false;
    }
    if !shared::keepalive::job_failed(job) {
        return false;
    }
    error!(
        toolset = labels
            .get("sycophant.md/toolset")
            .map(String::as_str)
            .unwrap_or("<unlabelled>"),
        job = job.metadata.name.as_deref().unwrap_or("<unnamed>"),
        "discovery Job failed; this toolset serves no tools until the controller rolls"
    );
    true
}

/// Watch tool Jobs and react to terminal/deleted ones. Selector is
/// broad (`app.kubernetes.io/part-of=sycophant`), so it also streams the
/// discovery Jobs, whose failures are reported. Jobs with no
/// `sycophant.md/tool` label are ignored by the tool-job handler.
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
                    handle_discovery_job_event(&job, deleted);
                    handle_tool_job_event(&state, &job, deleted).await;
                }
            }
        },
    )
    .await
}

pub async fn tool_cleanup_loop(state: Arc<ControllerState>) {
    loop {
        tokio::time::sleep(CLEANUP_INTERVAL).await;
        let expired = find_expired_jobs(&state, Instant::now()).await;
        remove_expired_jobs(&state, &expired).await;
    }
}

/// Re-adopt existing keepalive tool Jobs into the in-memory `active_jobs` map
/// on controller startup. Fires after the toolset watcher's first sync so
/// `get_toolset` resolves the per-toolset keepalive flag.
pub async fn reconcile_tool_jobs(
    client: &Client,
    namespace: &str,
    state: &ControllerState,
) -> Result<(), kube::Error> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels("app.kubernetes.io/part-of=sycophant");
    let list = jobs.list(&lp).await?;
    let mut adopted = 0usize;
    for job in list.items {
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
        let toolset_name = match labels.get("sycophant.md/toolset") {
            Some(c) => c.clone(),
            None => continue,
        };
        let workspace = match labels.get("sycophant.md/workspace") {
            Some(w) => w.clone(),
            None => continue,
        };
        let job_name = match job.metadata.name.clone() {
            Some(n) => n,
            None => continue,
        };
        let keepalive_seconds = match state.get_toolset(&toolset_name).await {
            Some(entry) if entry.keepalive => TOOL_KEEPALIVE_IDLE_SECONDS,
            _ => 0,
        };
        state
            .set_active_job(ActiveJob {
                job_name,
                // Not spawned by this controller: no call id names it, so the
                // record is not attachable and every GetToolCall is refused.
                job_id: String::new(),
                tool_name,
                workspace,
                last_activity: Instant::now(),
                keepalive_seconds,
                // Unknown provenance. Irrelevant to reuse: the empty job_id
                // above already forces delete-and-respawn on the next call.
                grant: None,
            })
            .await;
        adopted += 1;
    }
    if adopted > 0 {
        info!(
            count = adopted,
            "reconciled existing keepalive tool Jobs into active_jobs map"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tool_keepalive_tests {
    use super::*;
    use crate::state::{ToolResultGuard, RESULT_CHANNEL_CAPACITY};
    use proto_common::tool_result_frame::Frame;
    use proto_common::{ToolOutcome, ToolResultFrame};
    use tokio::sync::mpsc;

    fn make_state() -> Arc<ControllerState> {
        ControllerState::new(
            None,
            String::new(),
            String::new(),
            shared::scheduling::SchedulingConfig::default(),
        )
    }

    fn assert_error_terminal(frame: Option<ToolResultFrame>) {
        match frame.and_then(|f| f.frame) {
            Some(Frame::Complete(c)) => {
                assert_ne!(c.outcome(), ToolOutcome::Done, "terminal must be an error")
            }
            other => panic!("expected an error ToolComplete terminal, got {other:?}"),
        }
    }

    fn make_active_job(tool: &str, idle_secs: u64, keepalive_secs: u64) -> ActiveJob {
        ActiveJob {
            job_name: format!("tool-{tool}-abc"),
            job_id: format!("call-{tool}"),
            tool_name: tool.to_string(),
            workspace: "ws".to_string(),
            last_activity: Instant::now() - Duration::from_secs(idle_secs),
            keepalive_seconds: keepalive_secs,
            grant: None,
        }
    }

    #[tokio::test]
    async fn expired_job_removed() {
        let state = make_state();
        state
            .set_active_job(make_active_job("test-tool", 120, 60))
            .await;

        let expired = find_expired_jobs(&state, Instant::now()).await;
        remove_expired_jobs(&state, &expired).await;

        assert_eq!(state.active_job_count().await, 0);
    }

    #[tokio::test]
    async fn active_job_not_removed() {
        let state = make_state();
        state
            .set_active_job(make_active_job("active-tool", 0, 300))
            .await;

        let expired = find_expired_jobs(&state, Instant::now()).await;
        remove_expired_jobs(&state, &expired).await;

        assert_eq!(state.active_job_count().await, 1);
    }

    #[tokio::test]
    async fn zero_keepalive_never_expires() {
        let state = make_state();
        state
            .set_active_job(make_active_job("fire-forget", 9999, 0))
            .await;

        let expired = find_expired_jobs(&state, Instant::now()).await;
        assert!(expired.is_empty());
    }

    #[tokio::test]
    async fn multiple_expired_at_once() {
        let state = make_state();
        state
            .set_active_job(make_active_job("tool-a", 120, 60))
            .await;
        state
            .set_active_job(make_active_job("tool-b", 200, 60))
            .await;

        let expired = find_expired_jobs(&state, Instant::now()).await;
        assert_eq!(expired.len(), 2);

        remove_expired_jobs(&state, &expired).await;
        assert_eq!(state.active_job_count().await, 0);
    }

    #[tokio::test]
    async fn remove_expired_jobs_fails_parked_call() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        state
            .set_result_tx("call-1".into(), "ws".into(), "test-tool".into(), tx)
            .await;
        state
            .set_active_job(make_active_job("test-tool", 120, 60))
            .await;

        let expired = find_expired_jobs(&state, Instant::now()).await;
        remove_expired_jobs(&state, &expired).await;

        let frame = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("parked call must not hang after reap");
        assert_error_terminal(frame);
    }

    #[tokio::test]
    async fn tool_result_guard_drop_emits_error_terminal() {
        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        let guard = ToolResultGuard::new(tx);
        drop(guard);
        assert_error_terminal(rx.recv().await);
    }

    #[tokio::test]
    async fn tool_result_guard_marked_complete_drop_is_silent() {
        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        let mut guard = ToolResultGuard::new(tx);
        guard.mark_complete();
        drop(guard);
        assert!(
            rx.recv().await.is_none(),
            "a completed guard emits no synthetic terminal on drop"
        );
    }

    fn tool_job(tool: &str, status: k8s_openapi::api::batch::v1::JobStatus) -> Job {
        // Named as `make_active_job` names it, so the event matches the record.
        named_tool_job(tool, &format!("tool-{tool}-abc"), status)
    }

    fn named_tool_job(
        tool: &str,
        job_name: &str,
        status: k8s_openapi::api::batch::v1::JobStatus,
    ) -> Job {
        use std::collections::BTreeMap;
        let mut job = Job::default();
        let mut labels = BTreeMap::new();
        labels.insert(
            "app.kubernetes.io/part-of".to_string(),
            "sycophant".to_string(),
        );
        labels.insert("sycophant.md/tool".to_string(), tool.to_string());
        labels.insert("sycophant.md/workspace".to_string(), "ws".to_string());
        job.metadata.labels = Some(labels);
        job.metadata.name = Some(job_name.to_string());
        job.status = Some(status);
        job
    }

    #[tokio::test]
    async fn handle_tool_job_event_fails_parked_call_on_terminal() {
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = make_state();
        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        state
            .set_result_tx("call-1".into(), "ws".into(), "tool-x".into(), tx)
            .await;
        state.set_active_job(make_active_job("tool-x", 0, 60)).await;

        let j = tool_job(
            "tool-x",
            JobStatus {
                failed: Some(1),
                ..Default::default()
            },
        );
        assert!(
            handle_tool_job_event(&state, &j, false).await,
            "must act on terminal"
        );

        let frame = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("must not hang");
        assert_error_terminal(frame);
        assert_eq!(state.active_job_count().await, 0);
    }

    /// Every path that removes a record drains that key's calls, so no record
    /// means the drain already happened and anything parked now belongs to a
    /// successor. Breaks if the gate widens back to "not superseded", which
    /// terminates a live call the moment a predecessor's event lands.
    #[tokio::test]
    async fn handle_tool_job_event_leaves_calls_alone_when_no_record_remains() {
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = make_state();
        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        state
            .set_result_tx("call-1".into(), "ws".into(), "tool-x".into(), tx)
            .await;

        let j = tool_job(
            "tool-x",
            JobStatus {
                failed: Some(1),
                ..Default::default()
            },
        );
        assert!(
            !handle_tool_job_event(&state, &j, true).await,
            "an event finding no record has nothing of its own to retire"
        );

        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "the parked call belongs to whoever holds the key now"
        );
    }

    /// Replacing a Job for the same grant spawns the successor under the same
    /// `(workspace, tool, grant)` key. The predecessor's delete event must leave
    /// the successor's record and its parked call alone.
    #[tokio::test]
    async fn handle_tool_job_event_leaves_a_successor_record_alone() {
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = make_state();
        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        state
            .set_result_tx("call-2".into(), "ws".into(), "tool-x".into(), tx)
            .await;
        // The live record names the replacement Job.
        state.set_active_job(make_active_job("tool-x", 0, 60)).await;

        let predecessor = named_tool_job(
            "tool-x",
            "tool-tool-x-deadbeef",
            JobStatus {
                failed: Some(1),
                ..Default::default()
            },
        );
        assert!(
            !handle_tool_job_event(&state, &predecessor, true).await,
            "an event about an already-replaced Job must not act"
        );

        assert_eq!(
            state.active_job_count().await,
            1,
            "the successor's record must survive its predecessor's delete event"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "the successor's parked call must not be failed"
        );
    }

    #[tokio::test]
    async fn handle_tool_job_event_ignores_nonterminal_apply() {
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = make_state();
        let j = tool_job(
            "tool-x",
            JobStatus {
                active: Some(1),
                ..Default::default()
            },
        );
        assert!(!handle_tool_job_event(&state, &j, false).await);
    }

    // ---- Discovery-Job failure observation ----

    /// A discovery Job as `build_discovery_job` labels it: `sycophant.md/job`
    /// and `sycophant.md/toolset`, and deliberately NO `sycophant.md/tool`.
    fn discovery_job(toolset: &str, status: k8s_openapi::api::batch::v1::JobStatus) -> Job {
        use std::collections::BTreeMap;
        let mut job = Job::default();
        let mut labels = BTreeMap::new();
        labels.insert(
            "app.kubernetes.io/part-of".to_string(),
            "sycophant".to_string(),
        );
        labels.insert("sycophant.md/job".to_string(), "discovery".to_string());
        labels.insert("sycophant.md/toolset".to_string(), toolset.to_string());
        job.metadata.labels = Some(labels);
        job.metadata.name = Some(format!("discovery-{toolset}-abc"));
        job.status = Some(status);
        job
    }

    #[test]
    fn discovery_job_failure_is_reported() {
        use k8s_openapi::api::batch::v1::JobStatus;
        let job = discovery_job(
            "stdlib",
            JobStatus {
                failed: Some(1),
                ..Default::default()
            },
        );
        assert!(
            handle_discovery_job_event(&job, false),
            "a failed discovery Job must not be silent; its toolset stays empty"
        );
    }

    #[test]
    fn discovery_job_success_is_not_reported() {
        use k8s_openapi::api::batch::v1::JobStatus;
        let job = discovery_job(
            "stdlib",
            JobStatus {
                succeeded: Some(1),
                completion_time: None,
                ..Default::default()
            },
        );
        assert!(
            !handle_discovery_job_event(&job, false),
            "a successful discovery is terminal but not a failure; reporting it would fire on every healthy boot"
        );
    }

    #[test]
    fn discovery_job_delete_is_not_reported() {
        use k8s_openapi::api::batch::v1::JobStatus;
        let job = discovery_job(
            "stdlib",
            JobStatus {
                failed: Some(1),
                ..Default::default()
            },
        );
        assert!(
            !handle_discovery_job_event(&job, true),
            "ttlSecondsAfterFinished deletes every discovery Job; the delete must not re-report"
        );
    }

    /// Regression pin (green today): the tool-job handler must stay inert on
    /// discovery Jobs, which the `part-of=sycophant` watch selector also
    /// streams to it. Locks the `sycophant.md/tool` guard.
    #[tokio::test]
    async fn handle_tool_job_event_ignores_discovery_job() {
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = make_state();
        let job = discovery_job(
            "stdlib",
            JobStatus {
                failed: Some(1),
                ..Default::default()
            },
        );
        assert!(
            !handle_tool_job_event(&state, &job, false).await,
            "a discovery Job carries no sycophant.md/tool and must never touch active_jobs"
        );
    }
}
