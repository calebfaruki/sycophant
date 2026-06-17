//! Leaf primitives for Kubernetes Job keepalive lifecycle. Shared by
//! controllers that spawn long-lived chamber / LLM-worker pods and need
//! a uniform health probe + delete pattern.
//!
//! What's NOT here: the per-controller `cleanup_loop`, `reconcile_*`,
//! and the in-memory active-job map. Those depend on controller-shaped
//! state (key type, label vocabulary) and live in the consumer crates.

use std::time::Duration;

use k8s_openapi::api::batch::v1::Job;
use kube::api::{DeleteParams, PropagationPolicy};
use kube::{Api, Client};

/// Cold-start grace window for newly-created Jobs. While `status.active`
/// is set but `start_time` is within this window, `job_health` reports
/// `Pending`; past it, `Running`. Covers image-pull + gVisor sandbox
/// boot, which can run 30-50s on a stressed node.
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
/// would stall the caller on stuck-Terminating Pods (gVisor sandboxes
/// can wedge for tens of seconds). 404 from the apiserver is collapsed
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
