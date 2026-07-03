//! LLM-job idle keepalive. Mirrors the airlock-controller pattern: a
//! 30s tick scans every model slot, and any slot whose
//! `last_activity` is older than `KEEPALIVE_IDLE_SECONDS` has its k8s
//! Job deleted and its `job_connected` flag cleared so the next `turn`
//! RPC respawns a fresh pod.
//!
//! K8s-first ordering on delete: clearing state ahead of k8s would
//! leave the controller view ahead of the cluster ("not connected"
//! while the pod is still draining) and the next `turn` could spawn a
//! duplicate Job. K8s-first failure self-heals on the next tick; 404
//! collapses to success.

use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::api::batch::v1::Job;
use kube::api::ListParams;
use kube::{Api, Client};
use tracing::{error, info, warn};

use crate::state::ControllerState;

const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

/// Fail any turn parked on `model`'s slot. Takes the `ActiveTurn` and drops
/// its `TurnResultGuard`, emitting a terminal `TurnError` so the
/// transponder's parked `turn` stream ends instead of awaiting forever; the
/// transponder then originates the FAILED turn-state to the client. No-op
/// when no turn is loaded (the worker never pulled the assignment, or
/// already streamed its result).
async fn fail_active_turn(state: &ControllerState, model: &str) {
    let Some(active) = state.take_active_turn(model).await else {
        return;
    };
    // The guard's Drop emits the terminal TurnError onto the orphaned result
    // channel; the transponder (awaiting the turn stream) sees the end and
    // originates the FAILED turn-state to the client. hangar no longer delivers.
    drop(active);
}

/// Idle window before an LLM Job is reaped. Bumped by
/// `bump_model_activity` on every `get_turn` arrival and every
/// successful `stream_turn_result` Complete chunk. Matches the airlock
/// chamber default; per-model durations are deferred to a future
/// per-Model-CR field.
pub const KEEPALIVE_IDLE_SECONDS: Duration = Duration::from_secs(600);

pub async fn cleanup_loop(state: Arc<ControllerState>) {
    loop {
        tokio::time::sleep(CLEANUP_INTERVAL).await;
        sweep_idle(&state, Instant::now()).await;
    }
}

async fn sweep_idle(state: &ControllerState, now: Instant) {
    let expired = state.list_idle_models(KEEPALIVE_IDLE_SECONDS, now).await;
    if expired.is_empty() {
        return;
    }
    let client = match state.kube_client() {
        Some(c) => c.clone(),
        // Unit-test path: no kube wired. Drop state-only so tests
        // exercise the clearing semantics without an apiserver.
        None => {
            for (model, _) in &expired {
                state.set_active_llm_job(model, None).await;
                state.set_job_connected(model, false).await;
                fail_active_turn(state, model).await;
            }
            return;
        }
    };
    for (model, job_name) in expired {
        match shared::keepalive::delete_job(&client, state.namespace(), &job_name).await {
            Ok(()) => {
                info!(model = %model, job = %job_name, "deleted idle LLM keepalive Job");
                state.set_active_llm_job(&model, None).await;
                // Load-bearing: without this, the next `turn` RPC sees
                // `AlreadyConnected` on a slot whose Job no longer
                // exists and blocks on `wait_for_turn` forever.
                state.set_job_connected(&model, false).await;
                fail_active_turn(state, &model).await;
            }
            Err(kube::Error::Api(e)) => {
                error!(
                    code = e.code,
                    model = %model,
                    job = %job_name,
                    message = %e.message,
                    "delete refused, retrying next tick"
                );
            }
            Err(e) => {
                warn!(model = %model, job = %job_name, err = %e, "delete transient failure, retrying next tick");
            }
        }
    }
}

/// React to an LLM worker Job lifecycle event. On a terminal Job
/// (`job_is_terminal`) or a delete, clear the model slot's connection
/// flags so the next `turn` respawns, then fail any turn still parked on
/// the slot — the reactive complement to `sweep_idle`, catching a crashed
/// or externally-deleted worker in seconds instead of at the idle window.
/// Idempotent with `sweep_idle`: a turn already taken yields a no-op fail.
/// Non-terminal applies are ignored. Returns true if it acted. Exposed
/// for tests (drive it directly with a synthetic Job — no apiserver).
pub async fn handle_job_event(state: &ControllerState, job: &Job, deleted: bool) -> bool {
    if !deleted && !shared::keepalive::job_is_terminal(job) {
        return false;
    }
    let Some(model) = job
        .metadata
        .labels
        .as_ref()
        .and_then(|l| l.get("sycophant.md/model"))
        .cloned()
    else {
        return false;
    };
    state.set_active_llm_job(&model, None).await;
    state.set_job_connected(&model, false).await;
    fail_active_turn(state, &model).await;
    true
}

/// Watch LLM worker Jobs (label `sycophant.md/type=llm`) and react to
/// terminal/deleted Jobs via `handle_job_event`. Uses the existing
/// `batch/jobs: watch` RBAC grant — no new permission. Returns `Err` on a
/// stream error so `spawn_watcher_task` restarts it with backoff.
pub async fn watch_llm_jobs(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
) -> Result<(), String> {
    shared::keepalive::watch_jobs(client, namespace, "sycophant.md/type=llm", "llm", {
        move |job, deleted| {
            let state = state.clone();
            async move {
                handle_job_event(&state, &job, deleted).await;
            }
        }
    })
    .await
}

/// Re-adopt existing LLM Jobs into the per-model state map on
/// controller startup. Without this, after a controller restart the
/// next `turn` RPC would observe `AlreadyConnected=false`, spawn a
/// duplicate Job, and the old Job's pod would poll
/// `get_turn` forever for assignments the new controller never sends.
///
/// Must fire AFTER the model registry has been populated by the
/// watcher initial sync — otherwise `bump_model_activity` is a no-op
/// and adopted Jobs would be reaped on the first sweep.
pub async fn reconcile_active_jobs(
    client: &Client,
    namespace: &str,
    state: &ControllerState,
) -> Result<(), kube::Error> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels("sycophant.md/type=llm");
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
        let model_name = match labels.get("sycophant.md/model") {
            Some(m) => m.clone(),
            None => continue,
        };
        let job_name = match job.metadata.name.clone() {
            Some(n) => n,
            None => continue,
        };
        state.set_active_llm_job(&model_name, Some(job_name)).await;
        state.bump_model_activity(&model_name).await;
        adopted += 1;
    }
    if adopted > 0 {
        info!(count = adopted, "reconciled existing LLM keepalive Jobs");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{ModelSpec, ProviderRef};
    use shared::scheduling::SchedulingConfig;
    use tokio::sync::mpsc;

    fn make_state() -> ControllerState {
        ControllerState::new(
            None,
            "ns".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
            SchedulingConfig::default(),
        )
    }

    fn test_spec() -> ModelSpec {
        ModelSpec {
            provider_ref: ProviderRef {
                name: "anthropic".into(),
            },
            model: "claude-sonnet-4".into(),
            params: None,
        }
    }

    #[tokio::test]
    async fn slot_without_active_job_not_returned() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        let expired = state
            .list_idle_models(KEEPALIVE_IDLE_SECONDS, Instant::now())
            .await;
        assert!(expired.is_empty());
    }

    #[tokio::test]
    async fn recent_model_not_returned() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state
            .set_active_llm_job("m", Some("hangar-llm-m-abc".into()))
            .await;
        state.bump_model_activity("m").await;
        let expired = state
            .list_idle_models(KEEPALIVE_IDLE_SECONDS, Instant::now())
            .await;
        assert!(expired.is_empty());
    }

    #[tokio::test]
    async fn expired_model_returned() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state
            .set_active_llm_job("m", Some("hangar-llm-m-abc".into()))
            .await;
        let now = Instant::now() + KEEPALIVE_IDLE_SECONDS + Duration::from_secs(100);
        let expired = state.list_idle_models(KEEPALIVE_IDLE_SECONDS, now).await;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, "m");
        assert_eq!(expired[0].1, "hangar-llm-m-abc");
    }

    #[tokio::test]
    async fn sweep_clears_job_connected_when_kube_none() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state.set_job_connected("m", true).await;
        state
            .set_active_llm_job("m", Some("hangar-llm-m-abc".into()))
            .await;

        let now = Instant::now() + KEEPALIVE_IDLE_SECONDS + Duration::from_secs(1);
        sweep_idle(&state, now).await;

        let expired = state.list_idle_models(KEEPALIVE_IDLE_SECONDS, now).await;
        assert!(expired.is_empty(), "active_job_name should be cleared");
    }

    #[tokio::test]
    async fn boundary_at_exact_idle() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state
            .set_active_llm_job("m", Some("hangar-llm-m-abc".into()))
            .await;
        let now_at = Instant::now() + KEEPALIVE_IDLE_SECONDS;
        let expired = state.list_idle_models(KEEPALIVE_IDLE_SECONDS, now_at).await;
        assert_eq!(expired.len(), 1, "now == last + idle must be expired (>=)");

        let now_before = Instant::now() + KEEPALIVE_IDLE_SECONDS - Duration::from_secs(1);
        let expired = state
            .list_idle_models(KEEPALIVE_IDLE_SECONDS, now_before)
            .await;
        assert!(expired.is_empty(), "599s elapsed must not be expired");
    }

    #[tokio::test]
    async fn sweep_idle_fails_parked_turn() {
        // RED before the fix: sweep_idle reaps the Job but leaves the
        // ActiveTurn (and its result_tx) in the slot, so the parked
        // receiver never gets a terminal and recv() hangs forever — the
        // timeout converts that hang into a test failure. GREEN after: the
        // reap takes+drops the turn, firing the guard's TurnError.
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state
            .set_active_llm_job("m", Some("hangar-llm-m-abc".into()))
            .await;
        let (tx, mut rx) = mpsc::channel::<hangar_proto::TurnResultChunk>(4);
        state
            .set_active_turn("m", "ws".into(), "ws.c".into(), None, None, None, None, tx)
            .await;

        let now = Instant::now() + KEEPALIVE_IDLE_SECONDS + Duration::from_secs(1);
        sweep_idle(&state, now).await;

        let chunk = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("parked receiver must not hang after reap")
            .expect("reap must emit a terminal chunk");
        assert!(
            matches!(
                chunk.chunk,
                Some(hangar_proto::turn_result_chunk::Chunk::Error(_))
            ),
            "reaped turn must terminate with a TurnError"
        );
    }

    fn llm_job(model: &str, status: k8s_openapi::api::batch::v1::JobStatus) -> Job {
        use std::collections::BTreeMap;
        let mut job = Job::default();
        let mut labels = BTreeMap::new();
        labels.insert("sycophant.md/type".to_string(), "llm".to_string());
        labels.insert("sycophant.md/model".to_string(), model.to_string());
        job.metadata.labels = Some(labels);
        job.status = Some(status);
        job
    }

    #[tokio::test]
    async fn handle_job_event_fails_parked_turn_on_terminal() {
        // Reactive complement to sweep_idle: a Failed Job fails the parked
        // turn + clears flags immediately, no idle wait. Mutant: skip the
        // fail_active_turn call → the parked receiver hangs (timeout fails).
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state.set_job_connected("m", true).await;
        state
            .set_active_llm_job("m", Some("hangar-llm-m-abc".into()))
            .await;
        let (tx, mut rx) = mpsc::channel::<hangar_proto::TurnResultChunk>(4);
        state
            .set_active_turn("m", "ws".into(), "ws.c".into(), None, None, None, None, tx)
            .await;

        let job = llm_job(
            "m",
            JobStatus {
                failed: Some(1),
                ..Default::default()
            },
        );
        assert!(
            handle_job_event(&state, &job, false).await,
            "must act on terminal Job"
        );

        let chunk = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("must not hang")
            .expect("terminal expected");
        assert!(matches!(
            chunk.chunk,
            Some(hangar_proto::turn_result_chunk::Chunk::Error(_))
        ));
        assert!(
            state
                .list_idle_models(KEEPALIVE_IDLE_SECONDS, Instant::now())
                .await
                .is_empty(),
            "flags must be cleared so the next turn respawns"
        );
    }

    #[tokio::test]
    async fn handle_job_event_ignores_nonterminal_apply() {
        // A still-running Job (Apply, not terminal, not deleted) must not
        // fail the turn. Mutant: treat every Apply as terminal → live
        // turns get killed.
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        let job = llm_job(
            "m",
            JobStatus {
                active: Some(1),
                ..Default::default()
            },
        );
        assert!(!handle_job_event(&state, &job, false).await);
    }

    #[tokio::test]
    async fn handle_job_event_acts_on_delete_regardless_of_status() {
        // A Delete event must fail the turn even with no terminal status
        // (idle-GC / external kubectl delete). Mutant: gate on
        // job_is_terminal only (ignore `deleted`) → external deletes leak.
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state.set_job_connected("m", true).await;
        state
            .set_active_llm_job("m", Some("hangar-llm-m-abc".into()))
            .await;
        let (tx, mut rx) = mpsc::channel::<hangar_proto::TurnResultChunk>(4);
        state
            .set_active_turn("m", "ws".into(), "ws.c".into(), None, None, None, None, tx)
            .await;
        let job = llm_job("m", k8s_openapi::api::batch::v1::JobStatus::default());
        assert!(
            handle_job_event(&state, &job, true).await,
            "delete must act regardless of status"
        );
        let chunk = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("must not hang")
            .expect("terminal expected");
        assert!(matches!(
            chunk.chunk,
            Some(hangar_proto::turn_result_chunk::Chunk::Error(_))
        ));
    }
}
