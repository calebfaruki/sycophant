//! Leaf primitives for Kubernetes Job keepalive lifecycle. Shared by
//! controllers that spawn long-lived chamber / LLM-worker pods and need
//! a uniform health probe + delete pattern.
//!
//! What's NOT here: the per-controller `cleanup_loop`, `reconcile_*`,
//! and the in-memory active-job map. Those depend on controller-shaped
//! state (key type, label vocabulary) and live in the consumer crates.

use std::time::Duration;

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::batch::v1::Job;
use kube::api::{DeleteParams, PropagationPolicy};
use kube::runtime::watcher::{self, Event};
use kube::{Api, Client};

/// Cold-start grace window for newly-created Jobs. While `status.active`
/// is set but `start_time` is within this window, `job_health` reports
/// `Pending`; past it, `Running`. Covers image-pull plus, for gVisor
/// chamber pods, sandbox boot, which can run 30-50s on a stressed node.
pub const STARTUP_GRACE: Duration = Duration::from_secs(60);

/// Health snapshot of a keepalive Job. Drives the dedup decision in the
/// consumer controller: `Running` and within-grace `Pending` are
/// reusable; past-grace `Pending`, `Failed`, and `NotFound` all trigger
/// a delete + recreate.
#[derive(Debug)]
pub enum JobHealth {
    Running,
    Pending { age: Duration },
    Failed,
    NotFound,
}

/// Delete a Job by name with `Background` propagation. Background
/// returns immediately and lets the GC cascade to the Pod; `Foreground`
/// would stall the caller on stuck-Terminating Pods (a gVisor chamber
/// sandbox can wedge for tens of seconds). 404 from the apiserver is collapsed
/// to `Ok(())` — the Job is already gone, which is the desired end
/// state.
pub async fn delete_job(client: &Client, namespace: &str, name: &str) -> Result<(), kube::Error> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), namespace);
    let dp = DeleteParams {
        propagation_policy: Some(PropagationPolicy::Background),
        ..Default::default()
    };
    match jobs.delete(name, &dp).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
        Err(e) => Err(e),
    }
}

/// Probe a Job's current health. Used by consumers' dedup branch to
/// decide reuse vs delete+recreate. On any transient apiserver error
/// other than 404, returns `NotFound` — the safer direction (forces
/// recreate; worst case we spawn a duplicate that the next cleanup
/// tick reaps).
pub async fn job_health(client: &Client, namespace: &str, name: &str) -> JobHealth {
    let jobs: Api<Job> = Api::namespaced(client.clone(), namespace);
    let job = match jobs.get(name).await {
        Ok(j) => j,
        Err(kube::Error::Api(e)) if e.code == 404 => return JobHealth::NotFound,
        Err(_) => return JobHealth::NotFound,
    };

    let status = match &job.status {
        Some(s) => s,
        None => {
            return JobHealth::Pending {
                age: Duration::ZERO,
            }
        }
    };

    if status.failed.unwrap_or(0) > 0 {
        return JobHealth::Failed;
    }
    let failed_condition = status
        .conditions
        .as_ref()
        .map(|cs| cs.iter().any(|c| c.type_ == "Failed" && c.status == "True"))
        .unwrap_or(false);
    if failed_condition {
        return JobHealth::Failed;
    }

    if status.active.unwrap_or(0) == 0 && status.succeeded.unwrap_or(0) == 0 {
        // No active pod yet and not completed: still warming up.
        return JobHealth::Pending {
            age: Duration::ZERO,
        };
    }

    let age = status
        .start_time
        .as_ref()
        .map(|t| {
            let secs = k8s_openapi::jiff::Timestamp::now()
                .duration_since(t.0)
                .as_secs();
            if secs > 0 {
                Duration::from_secs(secs as u64)
            } else {
                Duration::ZERO
            }
        })
        .unwrap_or(Duration::ZERO);

    if age < STARTUP_GRACE {
        JobHealth::Pending { age }
    } else {
        JobHealth::Running
    }
}

/// True if a Job has reached a terminal state — failed or completed.
/// Used by the reactive Job watch to fail any in-flight turn/call the
/// instant its worker Job terminates, rather than waiting for the idle
/// sweep. Shares the failure-detection shape with `job_health` and adds
/// completion (succeeded / completion_time / `Complete` condition).
pub fn job_is_terminal(job: &Job) -> bool {
    let Some(status) = job.status.as_ref() else {
        return false;
    };
    if status.failed.unwrap_or(0) > 0 || status.succeeded.unwrap_or(0) > 0 {
        return true;
    }
    if status.completion_time.is_some() {
        return true;
    }
    status
        .conditions
        .as_ref()
        .map(|cs| {
            cs.iter()
                .any(|c| (c.type_ == "Failed" || c.type_ == "Complete") && c.status == "True")
        })
        .unwrap_or(false)
}

/// Watch Jobs matching `label_selector` and invoke `handler(job, deleted)`
/// on every Apply/InitApply (`deleted = false`) and Delete
/// (`deleted = true`). `component` names the watcher in the log/error
/// strings ("<component> job watcher ..."). Uses the existing
/// `batch/jobs: watch` grant — no new RBAC. Returns `Err` on a stream
/// error so `spawn_watcher_task` restarts it with backoff. The handler
/// receives an owned `Job` (the watcher already yields owned Jobs), which
/// keeps the returned future free of a borrow across `.await`.
pub async fn watch_jobs<F, Fut>(
    client: Client,
    namespace: &str,
    label_selector: &str,
    component: &str,
    handler: F,
) -> Result<(), String>
where
    F: Fn(Job, bool) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let api: Api<Job> = Api::namespaced(client, namespace);
    let cfg = watcher::Config::default().labels(label_selector);
    let mut stream = watcher::watcher(api, cfg).boxed();
    while let Some(event) = stream
        .try_next()
        .await
        .map_err(|e| format!("{component} job watcher error: {e}"))?
    {
        match event {
            Event::Apply(job) | Event::InitApply(job) => handler(job, false).await,
            Event::Delete(job) => handler(job, true).await,
            Event::Init | Event::InitDone => {}
        }
    }
    tracing::warn!("{component} job watcher stream ended");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::batch::v1::{JobCondition, JobStatus};

    fn job_with(status: JobStatus) -> Job {
        Job {
            status: Some(status),
            ..Default::default()
        }
    }

    #[test]
    fn terminal_on_failed_count() {
        // Mutant: flip the `failed > 0` check → a crashed worker reads as
        // non-terminal and the watch never fails its turn.
        assert!(job_is_terminal(&job_with(JobStatus {
            failed: Some(1),
            ..Default::default()
        })));
    }

    #[test]
    fn terminal_on_succeeded_count() {
        assert!(job_is_terminal(&job_with(JobStatus {
            succeeded: Some(1),
            ..Default::default()
        })));
    }

    #[test]
    fn terminal_on_failed_condition() {
        assert!(job_is_terminal(&job_with(JobStatus {
            conditions: Some(vec![JobCondition {
                type_: "Failed".into(),
                status: "True".into(),
                ..Default::default()
            }]),
            ..Default::default()
        })));
    }

    #[test]
    fn terminal_on_complete_condition() {
        assert!(job_is_terminal(&job_with(JobStatus {
            conditions: Some(vec![JobCondition {
                type_: "Complete".into(),
                status: "True".into(),
                ..Default::default()
            }]),
            ..Default::default()
        })));
    }

    #[test]
    fn not_terminal_when_active() {
        // Running worker (failed/succeeded zero, condition not True) must
        // NOT be treated as terminal — else the watch kills live turns.
        assert!(!job_is_terminal(&job_with(JobStatus {
            active: Some(1),
            conditions: Some(vec![JobCondition {
                type_: "Failed".into(),
                status: "False".into(),
                ..Default::default()
            }]),
            ..Default::default()
        })));
    }

    #[test]
    fn not_terminal_when_no_status() {
        assert!(!job_is_terminal(&Job::default()));
    }
}
