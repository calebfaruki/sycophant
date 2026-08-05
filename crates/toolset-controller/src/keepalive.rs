//! Idle keepalive for both worker Job kinds.
//!
//! Tool-worker Jobs are tracked per-tool in `active_jobs`; prompt-worker Jobs
//! are tracked per-model in the model slots. Each has its own 30s idle sweep
//! plus a reactive Job watch that fails parked work the instant a worker Job
//! goes terminal or is deleted. Both delete k8s-first: clearing state ahead of
//! the cluster would leave the controller's view ahead of reality and let the
//! next dispatch spawn a duplicate Job. K8s-first failure self-heals on the
//! next tick; 404 collapses to success.

use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::api::batch::v1::Job;
use kube::api::ListParams;
use kube::{Api, Client};
use tracing::{error, info, warn};

use crate::state::{ActiveJob, ControllerState, TurnResultGuard};
use shared::keepalive::delete_job;

const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

// =========================================================================
// Tool-worker keepalive
// =========================================================================

/// Idle window before a keepalive tool Job is reaped. Bumped by
/// `bump_last_activity` on every completed `stream_tool_result`, so a busy pod
/// with continuous traffic never expires.
pub const TOOL_KEEPALIVE_IDLE_SECONDS: u64 = 600;

pub async fn find_expired_jobs(
    state: &ControllerState,
    now: Instant,
) -> Vec<(String, String, String)> {
    state
        .list_active_jobs()
        .await
        .into_iter()
        .filter(|(_, _, keepalive_secs, last_activity)| {
            *keepalive_secs > 0 && now.duration_since(*last_activity).as_secs() >= *keepalive_secs
        })
        .map(|((workspace, tool_name), job_name, _, _)| (workspace, tool_name, job_name))
        .collect()
}

/// Fail any tool calls parked on `(workspace, tool_name)`. Dropping each drained
/// `ToolResultGuard` fires a synthetic error terminal frame, so a harness
/// streaming `AwaitToolResult` for a toolset we just tore down unblocks. Keyed
/// on the workspace too so one workspace's reap never fails another's calls.
async fn fail_pending_calls(state: &ControllerState, workspace: &str, tool_name: &str) {
    drop(state.take_result_txs_for_worker(workspace, tool_name).await);
}

/// Delete each expired tool Job from the kube API, then drop the matching
/// state entry (k8s first, then state).
pub async fn remove_expired_jobs(state: &ControllerState, expired: &[(String, String, String)]) {
    let client = state.kube_client().cloned();
    for (workspace, tool_name, job_name) in expired {
        match &client {
            Some(c) => match delete_job(c, state.namespace(), job_name).await {
                Ok(()) => {
                    info!(workspace = %workspace, tool = %tool_name, job = %job_name, "deleted idle keepalive Job");
                    state.remove_active_job(workspace, tool_name).await;
                    fail_pending_calls(state, workspace, tool_name).await;
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
                state.remove_active_job(workspace, tool_name).await;
                fail_pending_calls(state, workspace, tool_name).await;
            }
        }
    }

    if !expired.is_empty() {
        warn!(count = expired.len(), "cleaned up expired keepalive Jobs");
    }
}

/// React to a tool-worker Job lifecycle event. On a terminal or deleted Job,
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
    state.remove_active_job(&workspace, &tool).await;
    fail_pending_calls(state, &workspace, &tool).await;
    true
}

/// Watch tool-worker Jobs and react to terminal/deleted ones. Selector is
/// broad (`app.kubernetes.io/part-of=sycophant`); non-tool Jobs (no
/// `sycophant.md/tool` label) are ignored by the handler.
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
            Some(c) if c.spec.keepalive => TOOL_KEEPALIVE_IDLE_SECONDS,
            _ => 0,
        };
        state
            .set_active_job(ActiveJob {
                job_name,
                tool_name,
                workspace,
                last_activity: Instant::now(),
                keepalive_seconds,
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

// =========================================================================
// Prompt-worker keepalive
// =========================================================================

/// Idle window before a prompt-worker Job is reaped. Bumped by
/// `bump_model_activity` on every `get_turn` arrival and every successful
/// `stream_turn_result` Complete chunk.
pub const PROMPT_KEEPALIVE_IDLE: Duration = Duration::from_secs(600);

/// Reap every turn parked on `model`'s slot when its worker is gone: the loaded
/// `ActiveTurn` and any never-claimed `PendingTurn`. Both terminate their
/// result channel with a `TurnError` so the harness's parked stream ends, and
/// both drop their cancel token so the map cannot leak.
async fn reap_slot_turns(state: &ControllerState, model: &str) {
    fail_active_turn(state, model).await;
    fail_pending_turns(state, model).await;
}

async fn fail_active_turn(state: &ControllerState, model: &str) {
    let Some(active) = state.take_active_turn(model).await else {
        return;
    };
    state
        .finish_turn(&active.workspace, &active.conversation_id)
        .await;
    // The guard's Drop emits the terminal TurnError onto the orphaned result
    // channel; the harness sees the end and originates FAILED to the client.
    drop(active);
}

async fn fail_pending_turns(state: &ControllerState, model: &str) {
    for pending in state.drain_pending_turns(model).await {
        state
            .finish_turn(&pending.workspace, &pending.conversation_id)
            .await;
        drop(TurnResultGuard::new(pending.result_tx));
    }
}

pub async fn prompt_cleanup_loop(state: Arc<ControllerState>) {
    loop {
        tokio::time::sleep(CLEANUP_INTERVAL).await;
        sweep_idle(&state, Instant::now()).await;
    }
}

async fn sweep_idle(state: &ControllerState, now: Instant) {
    let expired = state.list_idle_models(PROMPT_KEEPALIVE_IDLE, now).await;
    if expired.is_empty() {
        return;
    }
    let client = match state.kube_client() {
        Some(c) => c.clone(),
        None => {
            for (model, _) in &expired {
                state.set_active_llm_job(model, None).await;
                state.set_job_connected(model, false).await;
                reap_slot_turns(state, model).await;
            }
            return;
        }
    };
    for (model, job_name) in expired {
        match shared::keepalive::delete_job(&client, state.namespace(), &job_name).await {
            Ok(()) => {
                info!(model = %model, job = %job_name, "deleted idle prompt keepalive Job");
                state.set_active_llm_job(&model, None).await;
                state.set_job_connected(&model, false).await;
                reap_slot_turns(state, &model).await;
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

/// React to a prompt-worker Job lifecycle event. On a terminal or deleted Job,
/// clear the model slot's connection flags so the next `turn` respawns, then
/// fail any turn still parked on the slot. Returns true if it acted.
pub async fn handle_prompt_job_event(state: &ControllerState, job: &Job, deleted: bool) -> bool {
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
    reap_slot_turns(state, &model).await;
    true
}

/// Watch prompt-worker Jobs (label `sycophant.md/type=prompt`) and react to
/// terminal/deleted Jobs. Uses the existing batch/jobs:watch RBAC grant.
pub async fn watch_prompt_jobs(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
) -> Result<(), String> {
    shared::keepalive::watch_jobs(client, namespace, "sycophant.md/type=prompt", "prompt", {
        move |job, deleted| {
            let state = state.clone();
            async move {
                handle_prompt_job_event(&state, &job, deleted).await;
            }
        }
    })
    .await
}

/// Re-adopt existing prompt Jobs into the per-model state on controller
/// startup. Must fire AFTER the model registry has been populated by the
/// watcher initial sync.
pub async fn reconcile_prompt_jobs(
    client: &Client,
    namespace: &str,
    state: &ControllerState,
) -> Result<(), kube::Error> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels("sycophant.md/type=prompt");
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
        info!(count = adopted, "reconciled existing prompt keepalive Jobs");
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
            "ghcr.io/test/prompt-job:latest".into(),
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
            tool_name: tool.to_string(),
            workspace: "ws".to_string(),
            last_activity: Instant::now() - Duration::from_secs(idle_secs),
            keepalive_seconds: keepalive_secs,
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
}

#[cfg(test)]
mod prompt_keepalive_tests {
    use super::*;
    use crate::crd::{ModelSpec, ProviderRef};
    use tokio::sync::mpsc;
    use toolset_proto::TurnResultChunk;

    fn make_state() -> Arc<ControllerState> {
        ControllerState::new(
            None,
            "ns".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/prompt-job:latest".into(),
            shared::scheduling::SchedulingConfig::default(),
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
            .list_idle_models(PROMPT_KEEPALIVE_IDLE, Instant::now())
            .await;
        assert!(expired.is_empty());
    }

    #[tokio::test]
    async fn recent_model_not_returned() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state
            .set_active_llm_job("m", Some("toolset-prompt-m-abc".into()))
            .await;
        state.bump_model_activity("m").await;
        let expired = state
            .list_idle_models(PROMPT_KEEPALIVE_IDLE, Instant::now())
            .await;
        assert!(expired.is_empty());
    }

    #[tokio::test]
    async fn expired_model_returned() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state
            .set_active_llm_job("m", Some("toolset-prompt-m-abc".into()))
            .await;
        let now = Instant::now() + PROMPT_KEEPALIVE_IDLE + Duration::from_secs(100);
        let expired = state.list_idle_models(PROMPT_KEEPALIVE_IDLE, now).await;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, "m");
        assert_eq!(expired[0].1, "toolset-prompt-m-abc");
    }

    #[tokio::test]
    async fn sweep_clears_job_connected_when_kube_none() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state.set_job_connected("m", true).await;
        state
            .set_active_llm_job("m", Some("toolset-prompt-m-abc".into()))
            .await;

        let now = Instant::now() + PROMPT_KEEPALIVE_IDLE + Duration::from_secs(1);
        sweep_idle(&state, now).await;

        let expired = state.list_idle_models(PROMPT_KEEPALIVE_IDLE, now).await;
        assert!(expired.is_empty(), "active_job_name should be cleared");
    }

    #[tokio::test]
    async fn boundary_at_exact_idle() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state
            .set_active_llm_job("m", Some("toolset-prompt-m-abc".into()))
            .await;
        let now_at = Instant::now() + PROMPT_KEEPALIVE_IDLE;
        let expired = state.list_idle_models(PROMPT_KEEPALIVE_IDLE, now_at).await;
        assert_eq!(expired.len(), 1, "now == last + idle must be expired (>=)");

        let now_before = Instant::now() + PROMPT_KEEPALIVE_IDLE - Duration::from_secs(1);
        let expired = state
            .list_idle_models(PROMPT_KEEPALIVE_IDLE, now_before)
            .await;
        assert!(expired.is_empty(), "599s elapsed must not be expired");
    }

    #[tokio::test]
    async fn sweep_idle_fails_parked_turn() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state
            .set_active_llm_job("m", Some("toolset-prompt-m-abc".into()))
            .await;
        let (tx, mut rx) = mpsc::channel::<TurnResultChunk>(4);
        state
            .set_active_turn("m", "ws".into(), "ws.c".into(), None, None, None, None, tx)
            .await;

        let now = Instant::now() + PROMPT_KEEPALIVE_IDLE + Duration::from_secs(1);
        sweep_idle(&state, now).await;

        let chunk = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("parked receiver must not hang after reap")
            .expect("reap must emit a terminal chunk");
        assert!(matches!(
            chunk.chunk,
            Some(toolset_proto::turn_result_chunk::Chunk::Error(_))
        ));
    }

    fn pending_turn(workspace: &str, conversation_id: &str) -> crate::state::PendingTurn {
        crate::state::PendingTurn {
            assignment: toolset_proto::TurnAssignment {
                system: None,
                tools: vec![],
                messages: vec![],
                params_json: None,
                conversation_id: conversation_id.into(),
            },
            result_tx: mpsc::channel::<TurnResultChunk>(64).0,
            workspace: workspace.into(),
            conversation_id: conversation_id.into(),
            reply_channel: None,
            role: None,
            correlation_id: None,
            system_prompt: None,
        }
    }

    #[tokio::test]
    async fn sweep_idle_reaps_never_claimed_pending_turn() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state
            .set_active_llm_job("m", Some("toolset-prompt-m-abc".into()))
            .await;

        state.register_cancel("ws", "ws.c").await;
        let mut pending = pending_turn("ws", "ws.c");
        let (tx, mut rx) = mpsc::channel::<TurnResultChunk>(64);
        pending.result_tx = tx;
        state.enqueue_turn("m", pending).await.unwrap();

        let now = Instant::now() + PROMPT_KEEPALIVE_IDLE + Duration::from_secs(1);
        sweep_idle(&state, now).await;

        assert!(
            state.cancel_token("ws", "ws.c").await.is_none(),
            "the never-claimed pending turn's cancel token must be reaped"
        );

        let chunk = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("parked receiver must not hang after reap")
            .expect("reap must emit a terminal chunk");
        assert!(matches!(
            chunk.chunk,
            Some(toolset_proto::turn_result_chunk::Chunk::Error(_))
        ));
    }

    fn prompt_job(model: &str, status: k8s_openapi::api::batch::v1::JobStatus) -> Job {
        use std::collections::BTreeMap;
        let mut job = Job::default();
        let mut labels = BTreeMap::new();
        labels.insert("sycophant.md/type".to_string(), "prompt".to_string());
        labels.insert("sycophant.md/model".to_string(), model.to_string());
        job.metadata.labels = Some(labels);
        job.status = Some(status);
        job
    }

    #[tokio::test]
    async fn handle_prompt_job_event_reaps_never_claimed_pending_turn() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state.set_job_connected("m", true).await;
        state
            .set_active_llm_job("m", Some("toolset-prompt-m-abc".into()))
            .await;

        state.register_cancel("ws", "ws.c").await;
        let mut pending = pending_turn("ws", "ws.c");
        let (tx, mut rx) = mpsc::channel::<TurnResultChunk>(64);
        pending.result_tx = tx;
        state.enqueue_turn("m", pending).await.unwrap();

        let job = prompt_job("m", k8s_openapi::api::batch::v1::JobStatus::default());
        assert!(
            handle_prompt_job_event(&state, &job, true).await,
            "delete must act"
        );

        assert!(
            state.cancel_token("ws", "ws.c").await.is_none(),
            "the never-claimed pending turn's cancel token must be reaped"
        );
        let chunk = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("parked receiver must not hang after reap")
            .expect("reap must emit a terminal chunk");
        assert!(matches!(
            chunk.chunk,
            Some(toolset_proto::turn_result_chunk::Chunk::Error(_))
        ));
    }

    #[tokio::test]
    async fn handle_prompt_job_event_fails_parked_turn_on_terminal() {
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state.set_job_connected("m", true).await;
        state
            .set_active_llm_job("m", Some("toolset-prompt-m-abc".into()))
            .await;
        let (tx, mut rx) = mpsc::channel::<TurnResultChunk>(4);
        state
            .set_active_turn("m", "ws".into(), "ws.c".into(), None, None, None, None, tx)
            .await;

        let job = prompt_job(
            "m",
            JobStatus {
                failed: Some(1),
                ..Default::default()
            },
        );
        assert!(
            handle_prompt_job_event(&state, &job, false).await,
            "must act on terminal Job"
        );

        let chunk = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("must not hang")
            .expect("terminal expected");
        assert!(matches!(
            chunk.chunk,
            Some(toolset_proto::turn_result_chunk::Chunk::Error(_))
        ));
        assert!(
            state
                .list_idle_models(PROMPT_KEEPALIVE_IDLE, Instant::now())
                .await
                .is_empty(),
            "flags must be cleared so the next turn respawns"
        );
    }

    #[tokio::test]
    async fn handle_prompt_job_event_ignores_nonterminal_apply() {
        use k8s_openapi::api::batch::v1::JobStatus;
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        let job = prompt_job(
            "m",
            JobStatus {
                active: Some(1),
                ..Default::default()
            },
        );
        assert!(!handle_prompt_job_event(&state, &job, false).await);
    }

    #[tokio::test]
    async fn handle_prompt_job_event_acts_on_delete_regardless_of_status() {
        let state = make_state();
        state.set_model_spec("m".into(), test_spec()).await;
        state.set_job_connected("m", true).await;
        state
            .set_active_llm_job("m", Some("toolset-prompt-m-abc".into()))
            .await;
        let (tx, mut rx) = mpsc::channel::<TurnResultChunk>(4);
        state
            .set_active_turn("m", "ws".into(), "ws.c".into(), None, None, None, None, tx)
            .await;
        let job = prompt_job("m", k8s_openapi::api::batch::v1::JobStatus::default());
        assert!(
            handle_prompt_job_event(&state, &job, true).await,
            "delete must act regardless of status"
        );
        let chunk = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("must not hang")
            .expect("terminal expected");
        assert!(matches!(
            chunk.chunk,
            Some(toolset_proto::turn_result_chunk::Chunk::Error(_))
        ));
    }
}
