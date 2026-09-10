//! In-process tool-call dispatch, relocated from the toolset controller into
//! the per-workspace harness.
//!
//! The harness both PRODUCES calls — the agent turn's `Source::Toolset` arm
//! drives [`DispatchState::begin_call`] — and DIALS the tool-job pods that run
//! them. Each call opens one bidirectional `ToolJob.Run` stream to the serving
//! pod ([`dial_and_run`]): the harness sends the assignment first, the pod
//! streams result frames back on the response half, and the harness pushes
//! cancel on the same outbound half. The pod never dials the harness, so a
//! popped tool pod can open no socket to the most privileged component.
//!
//! One workspace per harness, so the credential-containment isolation the
//! controller enforced across workspaces reduces here to per-grant isolation:
//! `(tool, grant)` keys keep two grants' Jobs and queued calls for one tool in
//! separate slots.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use proto_common::tool_result_frame::Frame;
use proto_common::{ToolComplete, ToolOutcome, ToolResultFrame};
use shared::scheduling::SchedulingConfig;
use shared::toolset::{CapabilityGrant, ToolsetConfig, WORKSPACE_MOUNT_PATH};
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use toolset_proto::run_tool_command::Command;
use toolset_proto::tool_job_client::ToolJobClient;
use toolset_proto::{RunToolCommand, ToolCallAssignment, ToolCancel};

use std::pin::Pin;

use model_provider::{
    assemble_turn_complete, error_turn_event, stream_event_to_turn_event, Format, LlmProvider,
    ProviderConfig, StreamEvent,
};
use toolset_proto::inference_job_client::InferenceJobClient;
use toolset_proto::run_inference_command::Command as InferenceCommandArm;
use toolset_proto::{RunInferenceCommand, TurnAssignment, TurnEvent, TurnRequest};
// Used only by the turn-source test modules, which reference it through `super`.
#[cfg(test)]
use toolset_proto::turn_event;

use crate::clients::{ToolsetRpc, TurnSource};
use crate::model_config::{ModelConfig, ModelConfigs};
use crate::{job, TOOL_JOB_PORT};

/// Bound on a tool call's in-flight frame channel. The dial driver forwards the
/// pod's frames into it; the producer's drain empties it.
pub(crate) const RESULT_CHANNEL_CAPACITY: usize = 64;

/// Bound on the dial-retry wait for a pod's per-pod DNS record to resolve and
/// accept a connection. Running work carries no time bound.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Idle window a keepalive tool pod stays warm between calls before it shuts
/// itself down. Zero for a one-shot (non-keepalive) tool.
const TOOL_KEEPALIVE_IDLE_SECONDS: u64 = 600;

/// RAII wrapper around a pending call's frame sender. Guarantees the producer's
/// drain always terminates: the pod forwards frames through `sender()` and, on
/// the terminal `ToolComplete`, `mark_complete` silences `Drop`. Dropped without
/// a terminal — a pod reaped or vanished mid-stream — it `try_send`s a synthetic
/// FAILED terminal so the drain unblocks instead of awaiting forever.
pub(crate) struct ToolResultGuard {
    tx: mpsc::Sender<ToolResultFrame>,
    complete: bool,
}

impl ToolResultGuard {
    fn new(tx: mpsc::Sender<ToolResultFrame>) -> Self {
        Self {
            tx,
            complete: false,
        }
    }

    fn sender(&self) -> &mpsc::Sender<ToolResultFrame> {
        &self.tx
    }

    fn mark_complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for ToolResultGuard {
    fn drop(&mut self) {
        if !self.complete {
            let _ = self.tx.try_send(ToolResultFrame {
                frame: Some(Frame::Complete(ToolComplete {
                    outcome: ToolOutcome::Failed as i32,
                    exit_code: -1,
                })),
            });
        }
    }
}

/// A call queued for a tool job, keyed in the pending map by `(tool, grant)`.
pub(crate) struct PendingCall {
    pub(crate) call_id: String,
    pub(crate) tool_name: String,
    /// Env-keyed argument map the pod runs, already validated against the tool's
    /// declared args — never the raw request JSON.
    pub(crate) args: HashMap<String, String>,
    pub(crate) working_dir: String,
    /// The grant this call selects; part of the queue key so a call for one
    /// grant never lands in another grant's bucket.
    pub(crate) grant: Option<String>,
    /// The Job this call may run on, as that pod names itself via
    /// `GetToolCallRequest.job_id`. Empty names no job, so nobody claims it.
    pub(crate) target_job_id: String,
}

/// What [`DispatchState::remove_active_job_named`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordEviction {
    Removed,
    NoRecord,
    SupersededByAnotherJob,
}

/// An active tool Job, keyed by `(tool, grant)` so two grants' Jobs for one tool
/// occupy distinct slots and neither can evict the other.
#[derive(Clone)]
pub(crate) struct ActiveJob {
    pub(crate) job_name: String,
    /// The call id that spawned the Job, which the pod presents back as
    /// `GetToolCallRequest.job_id`.
    pub(crate) job_id: String,
    pub(crate) tool_name: String,
    pub(crate) keepalive_seconds: u64,
    pub(crate) grant: Option<String>,
    pub(crate) last_activity: Instant,
}

type ActiveKey = (String, Option<String>);

/// Spawn config the producer needs to build and create a tool Job. Absent
/// (`kube_client` is `None`) in unit tests, where pods are simulated against the
/// state's own methods.
struct SpawnConfig {
    kube_client: Option<kube::Client>,
    namespace: String,
    /// The per-workspace headless Service the pods share as their `subdomain`.
    /// The harness dials a pod at `<job_id>.<service_name>.<namespace>...`.
    service_name: String,
    workspace: String,
    workspace_pvc: String,
    scheduling: SchedulingConfig,
    toolset_config: ToolsetConfig,
}

pub(crate) struct DispatchState {
    pending_calls: RwLock<HashMap<ActiveKey, Vec<PendingCall>>>,
    call_notify: Notify,
    result_txs: RwLock<HashMap<String, ToolResultGuard>>,
    /// `call_id -> tool_name`, the call's ownership shadow. Outlives the pod's
    /// connect (which takes the sender) and ends at `finish_call`.
    call_ids: RwLock<HashMap<String, String>>,
    call_cancel_tokens: RwLock<HashMap<String, CancellationToken>>,
    active_jobs: RwLock<HashMap<ActiveKey, ActiveJob>>,
    tool_dispatch_locks: RwLock<HashMap<String, Arc<Mutex<()>>>>,
    spawn: SpawnConfig,
}

impl DispatchState {
    pub(crate) fn new(
        kube_client: Option<kube::Client>,
        namespace: String,
        service_name: String,
        workspace: String,
        scheduling: SchedulingConfig,
        toolset_config: ToolsetConfig,
    ) -> Arc<Self> {
        let workspace_pvc = format!("workspace-data-{workspace}");
        Arc::new(Self {
            pending_calls: RwLock::new(HashMap::new()),
            call_notify: Notify::new(),
            result_txs: RwLock::new(HashMap::new()),
            call_ids: RwLock::new(HashMap::new()),
            call_cancel_tokens: RwLock::new(HashMap::new()),
            active_jobs: RwLock::new(HashMap::new()),
            tool_dispatch_locks: RwLock::new(HashMap::new()),
            spawn: SpawnConfig {
                kube_client,
                namespace,
                service_name,
                workspace,
                workspace_pvc,
                scheduling,
                toolset_config,
            },
        })
    }

    // ---- Call queue ----

    async fn enqueue_call(&self, call: PendingCall) {
        self.pending_calls
            .write()
            .await
            .entry((call.tool_name.clone(), call.grant.clone()))
            .or_default()
            .push(call);
        self.call_notify.notify_waiters();
    }

    /// Claim the first queued call admitted against `job_id`. The pod holds no
    /// grant, so this scans every grant bucket for `tool_name` and matches on
    /// `target_job_id`, which alone names the pod. An empty id matches nothing.
    /// Scans rather than head-peeks so an unclaimable entry cannot stall the
    /// claimable calls behind it.
    pub(crate) async fn dequeue_call(&self, tool_name: &str, job_id: &str) -> Option<PendingCall> {
        if job_id.is_empty() {
            return None;
        }
        let mut pending = self.pending_calls.write().await;
        for ((tool, _grant), calls) in pending.iter_mut() {
            if tool != tool_name {
                continue;
            }
            if let Some(index) = calls.iter().position(|c| c.target_job_id == job_id) {
                return Some(calls.remove(index));
            }
        }
        None
    }

    /// Take one still-queued call out, returning whether it was there. The ready
    /// deadline and the pod's `dequeue_call` take the same write lock, so exactly
    /// one removes the entry.
    async fn remove_pending_call(
        &self,
        tool_name: &str,
        grant: Option<&str>,
        call_id: &str,
    ) -> bool {
        let key = (tool_name.to_string(), grant.map(str::to_string));
        let mut pending = self.pending_calls.write().await;
        let Some(calls) = pending.get_mut(&key) else {
            return false;
        };
        let before = calls.len();
        calls.retain(|c| c.call_id != call_id);
        calls.len() != before
    }

    #[cfg(test)]
    fn call_waiter(&self) -> tokio::sync::futures::Notified<'_> {
        self.call_notify.notified()
    }

    // ---- Result channels ----

    async fn set_result_tx(
        &self,
        call_id: String,
        tool_name: String,
        tx: mpsc::Sender<ToolResultFrame>,
    ) {
        self.result_txs
            .write()
            .await
            .insert(call_id.clone(), ToolResultGuard::new(tx));
        self.call_ids.write().await.insert(call_id, tool_name);
    }

    /// Drain the result sender and read the call's shadow tool name. The shadow
    /// entry stays: it records the call and lives until `finish_call`.
    async fn take_result_tx(&self, call_id: &str) -> Option<(ToolResultGuard, String)> {
        let tx = self.result_txs.write().await.remove(call_id)?;
        let tool_name = self
            .call_ids
            .read()
            .await
            .get(call_id)
            .cloned()
            .unwrap_or_default();
        Some((tx, tool_name))
    }

    /// Retire every call bound to a torn-down tool job: queued entries first, so
    /// none can still be claimed, then the senders, whose guards fire terminals.
    async fn retire_calls_for_tool_job(&self, tool_name: &str) {
        let evicted: Vec<PendingCall> = {
            let mut pending = self.pending_calls.write().await;
            let keys: Vec<ActiveKey> = pending
                .keys()
                .filter(|(tool, _)| tool == tool_name)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|k| pending.remove(&k))
                .flatten()
                .collect()
        };
        drop(self.take_result_txs_for_tool_job(tool_name).await);
        for call in &evicted {
            self.fire_call_cancel(&call.call_id).await;
        }
    }

    async fn take_result_txs_for_tool_job(&self, tool_name: &str) -> Vec<ToolResultGuard> {
        let call_ids: Vec<String> = {
            let shadow = self.call_ids.read().await;
            shadow
                .iter()
                .filter(|(_, tool)| tool.as_str() == tool_name)
                .map(|(c, _)| c.clone())
                .collect()
        };
        let mut guards = Vec::with_capacity(call_ids.len());
        {
            let mut txs = self.result_txs.write().await;
            for call_id in &call_ids {
                if let Some(g) = txs.remove(call_id) {
                    guards.push(g);
                }
            }
        }
        {
            let mut shadow = self.call_ids.write().await;
            for call_id in &call_ids {
                shadow.remove(call_id);
            }
        }
        guards
    }

    // ---- Per-call cancellation ----

    async fn register_call_cancel(&self, call_id: String) {
        self.call_cancel_tokens
            .write()
            .await
            .insert(call_id, CancellationToken::new());
    }

    async fn call_cancel_token(&self, call_id: &str) -> Option<CancellationToken> {
        self.call_cancel_tokens.read().await.get(call_id).cloned()
    }

    pub(crate) async fn fire_call_cancel(&self, call_id: &str) -> bool {
        match self.call_cancel_tokens.write().await.remove(call_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Drop bookkeeping for a completed call: its cancel token and its ownership
    /// shadow. Idempotent.
    pub(crate) async fn finish_call(&self, call_id: &str) {
        self.call_cancel_tokens.write().await.remove(call_id);
        self.call_ids.write().await.remove(call_id);
    }

    // ---- Active tool Jobs ----

    async fn get_active_job(&self, tool_name: &str, grant: Option<&str>) -> Option<ActiveJob> {
        let key = (tool_name.to_string(), grant.map(str::to_string));
        self.active_jobs.read().await.get(&key).cloned()
    }

    pub(crate) async fn set_active_job(&self, job: ActiveJob) {
        let key = (job.tool_name.clone(), job.grant.clone());
        self.active_jobs.write().await.insert(key, job);
    }

    async fn remove_active_job_named(&self, tool_name: &str, job_name: &str) -> RecordEviction {
        let mut jobs = self.active_jobs.write().await;
        let mut matched_key = None;
        let mut superseded = false;
        for (key, job) in jobs.iter() {
            if key.0 == tool_name {
                if job.job_name == job_name {
                    matched_key = Some(key.clone());
                    break;
                }
                superseded = true;
            }
        }
        match matched_key {
            Some(key) => {
                jobs.remove(&key);
                RecordEviction::Removed
            }
            None if superseded => RecordEviction::SupersededByAnotherJob,
            None => RecordEviction::NoRecord,
        }
    }

    async fn bump_last_activity(&self, tool_name: &str) {
        let mut jobs = self.active_jobs.write().await;
        for (key, job) in jobs.iter_mut() {
            if key.0 == tool_name {
                job.last_activity = Instant::now();
            }
        }
    }

    #[cfg(test)]
    async fn active_job_count(&self) -> usize {
        self.active_jobs.read().await.len()
    }

    #[cfg(test)]
    pub(crate) async fn pending_call_count(&self) -> usize {
        self.pending_calls.read().await.values().map(Vec::len).sum()
    }

    async fn tool_dispatch_lock(&self, tool_name: &str) -> Arc<Mutex<()>> {
        if let Some(m) = self.tool_dispatch_locks.read().await.get(tool_name) {
            return m.clone();
        }
        self.tool_dispatch_locks
            .write()
            .await
            .entry(tool_name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    // ---- Producer: begin a call ----

    /// Dispatch a validated tool call. Spawns the tool Job (or attaches to the
    /// grant's existing one), enqueues the call, arms its ready deadline, and
    /// returns the tracking id with the receiver the arm drains. `args` is the
    /// env-keyed map, already validated against the tool's declared args.
    pub(crate) async fn begin_call(
        self: &Arc<Self>,
        tool_name: &str,
        toolset_name: &str,
        args: HashMap<String, String>,
        grant: Option<(&str, &CapabilityGrant)>,
    ) -> Result<(String, mpsc::Receiver<ToolResultFrame>), String> {
        let entry = self
            .spawn
            .toolset_config
            .get(toolset_name)
            .ok_or_else(|| format!("toolset {toolset_name} not found"))?
            .clone();

        let call_id = Uuid::new_v4().to_string();
        let working_dir = WORKSPACE_MOUNT_PATH.to_string();
        let call_grant = grant.as_ref().map(|(name, _)| name.to_string());

        let mut needs_deadline = false;
        let mut target_job_id = String::new();
        let mut target_job_name = String::new();

        {
            let dispatch_lock = self.tool_dispatch_lock(tool_name).await;
            let _guard = dispatch_lock.lock().await;

            if let Some(client) = self.spawn.kube_client.as_ref() {
                needs_deadline = true;
                let should_spawn = match self.get_active_job(tool_name, call_grant.as_deref()).await
                {
                    None => true,
                    // A record naming no job id is not attachable; respawn beside
                    // it. The harness holds no delete RBAC, so it leaves the old
                    // record to its ready deadline rather than deleting the pod.
                    Some(active) if active.job_id.is_empty() => true,
                    // A non-keepalive tool runs one Job per call: its pod exits
                    // after a single call, so its record must never be attached
                    // to. Spawn fresh and overwrite the record.
                    Some(active) if active.keepalive_seconds == 0 => true,
                    Some(active) => {
                        // The grant's warm pod long-polls GetToolCall on this
                        // harness; attach this call to it.
                        target_job_id = active.job_id.clone();
                        target_job_name = active.job_name.clone();
                        false
                    }
                };

                if should_spawn {
                    let job_spec = job::build_tool_job(
                        tool_name,
                        toolset_name,
                        &entry,
                        &call_id,
                        &self.spawn.namespace,
                        &self.spawn.service_name,
                        &self.spawn.workspace,
                        &self.spawn.workspace_pvc,
                        &self.spawn.scheduling,
                        grant,
                    );
                    let job_name = job_spec
                        .metadata
                        .name
                        .clone()
                        .expect("build_tool_job always sets metadata.name");
                    match tokio::time::timeout(
                        Duration::from_secs(10),
                        job::create_job(client, &self.spawn.namespace, &job_spec),
                    )
                    .await
                    {
                        Ok(Ok(_)) => {
                            info!(call_id = %call_id, tool = %tool_name, "tool Job created")
                        }
                        Ok(Err(e)) => return Err(format!("failed to create tool Job: {e}")),
                        Err(_) => return Err("k8s API timed out creating tool Job".to_string()),
                    }
                    target_job_name = job_name.clone();
                    self.set_active_job(ActiveJob {
                        job_name,
                        job_id: call_id.clone(),
                        tool_name: tool_name.to_string(),
                        keepalive_seconds: if entry.keepalive {
                            TOOL_KEEPALIVE_IDLE_SECONDS
                        } else {
                            0
                        },
                        grant: call_grant.clone(),
                        last_activity: Instant::now(),
                    })
                    .await;
                    target_job_id = call_id.clone();
                }
            } else {
                target_job_id = self
                    .get_active_job(tool_name, call_grant.as_deref())
                    .await
                    .map(|active| active.job_id)
                    .unwrap_or_default();
            }
        }

        let (result_tx, result_rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        self.set_result_tx(call_id.clone(), tool_name.to_string(), result_tx)
            .await;
        self.register_call_cancel(call_id.clone()).await;
        self.enqueue_call(PendingCall {
            call_id: call_id.clone(),
            tool_name: tool_name.to_string(),
            args,
            working_dir,
            grant: call_grant.clone(),
            target_job_id: target_job_id.clone(),
        })
        .await;

        info!(call_id = %call_id, tool = %tool_name, "call enqueued");

        // The publish above runs unlocked, so the job can be retired under it.
        // `remove_pending_call` arbitrates: losing it means a job took the call.
        let retired = target_job_id.is_empty()
            || self
                .get_active_job(tool_name, call_grant.as_deref())
                .await
                .is_none_or(|active| active.job_id != target_job_id);
        if retired
            && self
                .remove_pending_call(tool_name, call_grant.as_deref(), &call_id)
                .await
        {
            self.finish_call(&call_id).await;
            drop(self.take_result_tx(&call_id).await);
            return Err(format!(
                "the tool job for {tool_name} was retired before the call reached it"
            ));
        }

        if needs_deadline {
            self.spawn_dial_driver(tool_name.to_string(), target_job_id, target_job_name);
        }

        Ok((call_id, result_rx))
    }

    /// Dial this call's serving pod and run it to completion. Reproduces the
    /// pod's old pull-loop on the harness side: claim the enqueued call, dial its
    /// per-pod DNS name with retry for the readiness window, hand [`dial_and_run`]
    /// the call's frame sender and cancel token, and on a dial that never
    /// connects evict the stale active-job record so the next call spawns fresh
    /// rather than re-attaching to a pod that never answered. The harness holds
    /// no delete RBAC, so it never deletes the Job — its own
    /// `ttlSecondsAfterFinished` reaps it.
    fn spawn_dial_driver(
        self: &Arc<Self>,
        tool_name: String,
        target_job_id: String,
        job_name: String,
    ) {
        let state = self.clone();
        tokio::spawn(async move {
            // The pod claims one call per connection, scanning every grant
            // bucket by the job id it was spawned as. A concurrent driver may
            // take the peer call; its own guard then fires that call's terminal.
            let Some(call) = state.dequeue_call(&tool_name, &target_job_id).await else {
                return;
            };
            let call_id = call.call_id;
            let assignment = ToolCallAssignment {
                call_id: call_id.clone(),
                working_dir: call.working_dir,
                args: call.args,
            };
            let cancel = state.call_cancel_token(&call_id).await.unwrap_or_default();
            let Some((mut guard, tool)) = state.take_result_tx(&call_id).await else {
                return;
            };
            let sender = guard.sender().clone();
            // `dial_and_run` owns the terminal guarantee for this call from here,
            // so the parked state guard must not also fire one.
            guard.mark_complete();
            drop(guard);

            let endpoint = format!(
                "http://{}:{TOOL_JOB_PORT}",
                pod_dial_name(
                    &target_job_id,
                    &state.spawn.service_name,
                    &state.spawn.namespace
                )
            );
            let completed = dial_and_run(endpoint, assignment, sender, cancel, READY_TIMEOUT).await;
            state.finish_call(&call_id).await;
            if completed {
                state.bump_last_activity(&tool).await;
            } else {
                warn!(
                    call_id = %call_id,
                    tool = %tool_name,
                    job = %job_name,
                    "tool pod never answered within readyTimeout; failing the call and evicting its record"
                );
                if state.remove_active_job_named(&tool_name, &job_name).await
                    == RecordEviction::Removed
                {
                    state.retire_calls_for_tool_job(&tool_name).await;
                }
            }
        });
    }

    /// Drain a frame stream into a parked call's guard the way [`dial_and_run`]
    /// drains a dialed pod's response half, then retire the call. A test seam for
    /// the await fan-out, conversation-persistence, and abnormal-end paths, which
    /// feed frames directly rather than standing up a full serving pod: a frame
    /// stream error returns early so the guard drops unmarked and synthesizes the
    /// FAILED terminal, mirroring a pod stream that resets without a terminal.
    #[cfg(test)]
    pub(crate) async fn forward_result_frames<S>(
        &self,
        call_id: String,
        mut stream: S,
    ) -> Result<(), tonic::Status>
    where
        S: tokio_stream::Stream<Item = Result<ToolResultFrame, tonic::Status>> + Unpin,
    {
        let Some((mut guard, tool_name)) = self.take_result_tx(&call_id).await else {
            return Ok(());
        };

        let mut saw_terminal = false;
        while let Some(frame) = stream.next().await {
            let frame = frame?;
            if matches!(frame.frame, Some(Frame::Complete(_))) {
                saw_terminal = true;
            }
            let _ = guard.sender().send(frame).await;
        }

        if saw_terminal {
            guard.mark_complete();
        }
        drop(guard);

        self.finish_call(&call_id).await;
        if !tool_name.is_empty() {
            self.bump_last_activity(&tool_name).await;
        }
        Ok(())
    }
}

/// The per-pod headless DNS name the harness dials for a call. Pure: it derives
/// the name from the call id the harness already holds, the workspace headless
/// Service, and the namespace, with no apiserver read. The tool pod's `hostname`
/// is its spawning `job_id` and its `subdomain` is the Service, so Kubernetes
/// publishes exactly this record once the pod is Ready.
pub(crate) fn pod_dial_name(job_id: &str, service: &str, namespace: &str) -> String {
    format!("{job_id}.{service}.{namespace}.svc.cluster.local")
}

/// Bound on the wait between dial attempts while a pod's per-pod record has not
/// yet resolved.
const DIAL_BACKOFF: Duration = Duration::from_millis(100);

/// Upper bound on a single dial attempt, so an address that resolves but never
/// answers cannot hang a driver past its readiness window.
const DIAL_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);

/// Dial the serving pod and run one tool call to completion over a single
/// `ToolJob.Run` stream. Sends the assignment as the first message, forwards the
/// pod's result frames onto `result_tx`, and pushes a cancel on the same
/// outbound half when `cancel` fires. Retries the dial with backoff for
/// `ready_window`; if the pod never answers within it, or the frame stream ends
/// or resets without a terminal `Complete`, the held [`ToolResultGuard`] drops
/// and fires the synthetic FAILED terminal the parked drain already understands.
/// Returns whether a terminal `Complete` arrived.
async fn dial_and_run(
    endpoint: String,
    assignment: ToolCallAssignment,
    result_tx: mpsc::Sender<ToolResultFrame>,
    cancel: CancellationToken,
    ready_window: Duration,
) -> bool {
    // Owns this call's terminal guarantee from here: any early return below
    // drops it, firing the synthetic FAILED so the drain never awaits forever.
    let mut guard = ToolResultGuard::new(result_tx);

    let deadline = Instant::now() + ready_window;
    let mut client = loop {
        if cancel.is_cancelled() {
            return false;
        }
        match tokio::time::timeout(
            DIAL_ATTEMPT_TIMEOUT,
            ToolJobClient::connect(endpoint.clone()),
        )
        .await
        {
            Ok(Ok(client)) => break client,
            _ => {
                if Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(DIAL_BACKOFF).await;
            }
        }
    };

    // The outbound half: the assignment first, then a cancel if the call is
    // cancelled while it runs. Held on its own task so the inbound frame drain
    // runs concurrently on the same stream.
    let (out_tx, out_rx) = mpsc::channel::<RunToolCommand>(4);
    let cancel_out = cancel.clone();
    let out_task = tokio::spawn(async move {
        if out_tx
            .send(RunToolCommand {
                command: Some(Command::Assignment(assignment)),
            })
            .await
            .is_err()
        {
            return;
        }
        cancel_out.cancelled().await;
        let _ = out_tx
            .send(RunToolCommand {
                command: Some(Command::Cancel(ToolCancel {})),
            })
            .await;
    });

    let response = match client.run(ReceiverStream::new(out_rx)).await {
        Ok(response) => response,
        Err(_) => {
            out_task.abort();
            return false;
        }
    };

    let mut frames = response.into_inner();
    let mut saw_terminal = false;
    while let Some(frame) = frames.next().await {
        let Ok(frame) = frame else { break };
        let terminal = matches!(frame.frame, Some(Frame::Complete(_)));
        let _ = guard.sender().send(frame).await;
        if terminal {
            saw_terminal = true;
            break;
        }
    }
    out_task.abort();

    if saw_terminal {
        guard.mark_complete();
    }
    saw_terminal
}

/// The in-cluster base URL of a warm local model's ClusterIP Service. Pure: the
/// harness dials `inference-<key>` directly, so it derives the address from the
/// model key, the namespace, and the Service's serving port with no apiserver
/// read. Mirrors [`pod_dial_name`] for the local (no-job) path.
fn local_service_base_url(key: &str, namespace: &str, port: u16) -> String {
    format!("http://inference-{key}.{namespace}.svc.cluster.local:{port}/v1")
}

/// Parse a configured wire-format string into a provider [`Format`]. Reuses the
/// serde derive so the accepted spellings match the model config exactly.
fn parse_format(format: &str) -> Result<Format, String> {
    serde_json::from_str::<Format>(&format!("\"{format}\""))
        .map_err(|_| format!("unknown model format {format:?}"))
}

/// A turn source that yields exactly one `TurnError` and then ends. Used for
/// fail-closed model resolution (no model named, unknown model, unusable
/// format) and for a remote dial that never connects.
struct ErrorTurnSource {
    message: Option<String>,
}

impl ErrorTurnSource {
    fn new(message: String) -> Self {
        Self {
            message: Some(message),
        }
    }
}

#[async_trait::async_trait]
impl TurnSource for ErrorTurnSource {
    async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
        self.message.take().map(|m| Ok(error_turn_event(m)))
    }
}

/// A boxed, pinned provider event stream, exactly as `LlmProvider::call`
/// returns it.
type ProviderStream = Pin<Box<dyn tokio_stream::Stream<Item = Result<StreamEvent, String>> + Send>>;

/// A turn source backed by an in-process provider call to a warm local model.
/// The provider stream is opened lazily on the first `next_event`, so an
/// open error surfaces as a per-turn `TurnError` frame rather than a hard
/// dispatch failure. Streamed deltas are forwarded to the activity sink; the
/// terminal `Done` (or an early end of stream) is assembled into the one
/// authoritative `TurnComplete` the turn loop reads its result from.
struct ProviderTurnSource {
    provider: Box<dyn LlmProvider>,
    config: ProviderConfig,
    system: Option<String>,
    tools: Vec<proto_common::ToolDefinition>,
    messages: Vec<proto_common::Message>,
    stream: Option<ProviderStream>,
    opened: bool,
    events: Vec<StreamEvent>,
    finished: bool,
}

impl ProviderTurnSource {
    fn new(provider: Box<dyn LlmProvider>, config: ProviderConfig, request: TurnRequest) -> Self {
        Self {
            provider,
            config,
            system: request.system,
            tools: request.tools,
            messages: request.messages,
            stream: None,
            opened: false,
            events: Vec::new(),
            finished: false,
        }
    }
}

#[async_trait::async_trait]
impl TurnSource for ProviderTurnSource {
    async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
        if self.finished {
            return None;
        }
        if !self.opened {
            self.opened = true;
            match self
                .provider
                .call(
                    &self.messages,
                    self.system.as_deref(),
                    &self.tools,
                    None,
                    &self.config,
                )
                .await
            {
                Ok(stream) => self.stream = Some(stream),
                Err(e) => {
                    self.finished = true;
                    return Some(Ok(error_turn_event(format!("model call failed: {e}"))));
                }
            }
        }
        let stream = self.stream.as_mut().expect("stream opened above");
        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    self.events.push(event.clone());
                    if let StreamEvent::Done { stop_reason } = &event {
                        self.finished = true;
                        return Some(Ok(assemble_turn_complete(&self.events, stop_reason)));
                    }
                    if let Some(frame) = stream_event_to_turn_event(&event) {
                        return Some(Ok(frame));
                    }
                    // Thinking deltas carry no forwarded frame; keep reading.
                }
                Some(Err(e)) => {
                    self.finished = true;
                    return Some(Ok(error_turn_event(format!("model stream error: {e}"))));
                }
                None => {
                    self.finished = true;
                    return Some(Ok(error_turn_event(
                        "model stream ended without completion".to_string(),
                    )));
                }
            }
        }
    }
}

/// A turn source backed by a dialed inference-runtime pod. Holds the outbound
/// half open for the life of the source: dropping the source closes it, which
/// the pod reads as a cancel-by-disconnect. The response half is the same
/// `TurnEvent` stream the turn loop already consumes.
struct RemoteTurnSource {
    frames: tonic::Streaming<TurnEvent>,
    // Held so the outbound half stays open; Drop closes it and the pod ends.
    _out_tx: mpsc::Sender<RunInferenceCommand>,
}

#[async_trait::async_trait]
impl TurnSource for RemoteTurnSource {
    async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
        self.frames
            .next()
            .await
            .map(|r| r.map_err(|e| format!("stream error: {e}")))
    }
}

/// Dial a per-call inference-runtime pod and open its `InferenceJob.Run` stream.
/// Retries the dial with backoff for the readiness window, sends the assignment
/// as the first outbound message, and returns the response stream wrapped so the
/// turn loop reads model events straight off it. A dial that never connects, or
/// a Run that the pod refuses, returns `Err` — the caller turns that into a
/// per-turn error, not a restart.
async fn dial_inference(
    endpoint: String,
    assignment: TurnAssignment,
    ready_window: Duration,
) -> Result<RemoteTurnSource, String> {
    let deadline = Instant::now() + ready_window;
    let mut client = loop {
        match tokio::time::timeout(
            DIAL_ATTEMPT_TIMEOUT,
            InferenceJobClient::connect(endpoint.clone()),
        )
        .await
        {
            Ok(Ok(client)) => break client,
            _ => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "inference pod at {endpoint} did not answer within the readiness window"
                    ));
                }
                tokio::time::sleep(DIAL_BACKOFF).await;
            }
        }
    };

    let (out_tx, out_rx) = mpsc::channel::<RunInferenceCommand>(4);
    out_tx
        .send(RunInferenceCommand {
            command: Some(InferenceCommandArm::Assignment(assignment)),
        })
        .await
        .map_err(|_| "inference outbound half closed before the assignment was sent".to_string())?;

    let response = client
        .run(ReceiverStream::new(out_rx))
        .await
        .map_err(|e| format!("inference Run failed: {e}"))?;

    Ok(RemoteTurnSource {
        frames: response.into_inner(),
        _out_tx: out_tx,
    })
}

/// The harness's model-call seam. Implements the same `ToolsetRpc::turn` the
/// turn loop drives, but resolves the turn's model against the harness's own
/// catalog and dials the model directly: a warm local Service in-process, or a
/// per-call inference-runtime Job the harness creates and dials. An absent or
/// unknown model fails the turn closed — there is no default model.
pub(crate) struct InferenceDispatch {
    models: ModelConfigs,
    kube: Option<kube::Client>,
    namespace: String,
    service_name: String,
    workspace: String,
    scheduling: SchedulingConfig,
}

impl InferenceDispatch {
    pub(crate) fn new(
        models: ModelConfigs,
        kube: Option<kube::Client>,
        namespace: String,
        service_name: String,
        workspace: String,
        scheduling: SchedulingConfig,
    ) -> Self {
        Self {
            models,
            kube,
            namespace,
            service_name,
            workspace,
            scheduling,
        }
    }

    /// Open the model call for a resolved model config, returning a turn source.
    async fn open(
        &self,
        key: &str,
        config: &ModelConfig,
        request: TurnRequest,
    ) -> Box<dyn TurnSource> {
        let format = match parse_format(config.format()) {
            Ok(format) => format,
            Err(e) => return Box::new(ErrorTurnSource::new(e)),
        };
        match config {
            ModelConfig::Local { port, .. } => {
                let base_url = local_service_base_url(key, &self.namespace, *port);
                let provider = format.build(&base_url);
                let provider_config = ProviderConfig {
                    model: config.model().to_string(),
                    api_key: String::new(),
                };
                Box::new(ProviderTurnSource::new(provider, provider_config, request))
            }
            ModelConfig::Remote {
                base_url,
                image,
                secret,
                ..
            } => {
                let Some(kube) = self.kube.clone() else {
                    return Box::new(ErrorTurnSource::new(
                        "a remote model needs a kube client to create its inference job"
                            .to_string(),
                    ));
                };
                let call_id = Uuid::new_v4().to_string();
                let job_spec = job::build_inference_job(
                    &call_id,
                    &self.namespace,
                    &self.service_name,
                    &self.workspace,
                    key,
                    &self.scheduling,
                    image,
                    config.format(),
                    config.model(),
                    base_url,
                    secret.as_deref(),
                );
                if let Err(e) = job::create_job(&kube, &self.namespace, &job_spec).await {
                    return Box::new(ErrorTurnSource::new(format!(
                        "failed to create inference job: {e}"
                    )));
                }
                let endpoint = format!(
                    "http://{}:{TOOL_JOB_PORT}",
                    pod_dial_name(&call_id, &self.service_name, &self.namespace)
                );
                let assignment = TurnAssignment {
                    system: request.system,
                    tools: request.tools,
                    messages: request.messages,
                    conversation_id: request.conversation_id,
                };
                match dial_inference(endpoint, assignment, READY_TIMEOUT).await {
                    Ok(source) => Box::new(source),
                    Err(e) => Box::new(ErrorTurnSource::new(e)),
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl ToolsetRpc for InferenceDispatch {
    async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
        let Some(key) = request.model.clone() else {
            return Ok(Box::new(ErrorTurnSource::new(
                "no model named for this turn; the harness refuses to default a model".to_string(),
            )));
        };
        let Some(config) = self.models.get(&key).cloned() else {
            return Ok(Box::new(ErrorTurnSource::new(format!(
                "unknown model {key:?}; configured models: {:?}",
                self.models.names()
            ))));
        };
        Ok(self.open(&key, &config, request).await)
    }

    async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
        // The harness dialed the model directly and owns each turn's connection;
        // dropping the turn source closes it, so a local turn cancels by the turn
        // loop dropping the source. No separate broker call to make.
        Ok(())
    }

    async fn watch_tools(&mut self) -> Result<tonic::Streaming<toolset_proto::ToolList>, String> {
        Err("InferenceDispatch serves model turns, not the tool-catalog watch".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::toolset::ToolsetEntry;

    fn state() -> Arc<DispatchState> {
        DispatchState::new(
            None,
            "ns".to_string(),
            "http://harness:9090".to_string(),
            "ws".to_string(),
            SchedulingConfig::default(),
            ToolsetConfig::empty(),
        )
    }

    fn active(tool: &str, job_id: &str, grant: Option<&str>) -> ActiveJob {
        ActiveJob {
            job_name: format!("tool-{tool}-{job_id}"),
            job_id: job_id.to_string(),
            tool_name: tool.to_string(),
            keepalive_seconds: 600,
            grant: grant.map(str::to_string),
            last_activity: Instant::now(),
        }
    }

    fn queued(call_id: &str, tool: &str, job_id: &str) -> PendingCall {
        PendingCall {
            call_id: call_id.to_string(),
            tool_name: tool.to_string(),
            args: HashMap::new(),
            working_dir: "/w".to_string(),
            grant: None,
            target_job_id: job_id.to_string(),
        }
    }

    /// Two grants' active Jobs for one tool occupy distinct slots: neither can
    /// evict the other. The credential-containment property.
    #[tokio::test]
    async fn distinct_grants_for_one_tool_occupy_distinct_slots() {
        let state = state();
        state
            .set_active_job(active("Search", "a", Some("grant-a")))
            .await;
        state
            .set_active_job(active("Search", "b", Some("grant-b")))
            .await;
        assert_eq!(state.active_job_count().await, 2);
    }

    #[tokio::test]
    async fn a_job_cannot_claim_a_call_admitted_against_another_job() {
        let state = state();
        state.enqueue_call(queued("call-a", "shell", "job-a")).await;
        assert!(state.dequeue_call("shell", "job-b").await.is_none());
        assert_eq!(
            state.dequeue_call("shell", "job-a").await.unwrap().call_id,
            "call-a"
        );
    }

    #[tokio::test]
    async fn an_unclaimable_entry_does_not_block_the_calls_behind_it() {
        let state = state();
        state
            .enqueue_call(queued("orphan", "shell", "dead-job"))
            .await;
        state.enqueue_call(queued("live", "shell", "job-a")).await;
        assert_eq!(
            state.dequeue_call("shell", "job-a").await.unwrap().call_id,
            "live"
        );
    }

    #[tokio::test]
    async fn a_call_naming_no_job_is_claimed_by_nobody() {
        let state = state();
        state.enqueue_call(queued("targetless", "shell", "")).await;
        assert!(state.dequeue_call("shell", "job-a").await.is_none());
        assert!(state.dequeue_call("shell", "").await.is_none());
    }

    /// Breaks without the `enable()`: the enqueue's notify lands before the
    /// waiter registers and is lost, because `notify_waiters` stores no permit.
    #[tokio::test]
    async fn a_notify_after_enable_is_not_lost() {
        let state = state();
        let waiter = state.call_waiter();
        tokio::pin!(waiter);
        waiter.as_mut().enable();
        state.enqueue_call(queued("c", "t", "job-c")).await;
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("an enqueue after enable must wake the waiter");
    }

    /// The stale record is evicted so the tool's grant slot is free to spawn
    /// fresh, and any queued call bound to the dead pod is retired.
    #[tokio::test]
    async fn remove_active_job_named_matches_only_that_job() {
        let state = state();
        state.set_active_job(active("shell", "job-a", None)).await;
        assert_eq!(
            state
                .remove_active_job_named("shell", "tool-shell-job-b")
                .await,
            RecordEviction::SupersededByAnotherJob
        );
        assert_eq!(
            state
                .remove_active_job_named("shell", "tool-shell-job-a")
                .await,
            RecordEviction::Removed
        );
        assert_eq!(
            state
                .remove_active_job_named("shell", "tool-shell-job-a")
                .await,
            RecordEviction::NoRecord
        );
    }

    /// A dropped guard with no terminal fires a synthetic FAILED so a parked
    /// drain unblocks.
    #[tokio::test]
    async fn dropped_guard_fires_synthetic_failed_terminal() {
        let state = state();
        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        state
            .set_result_tx("call-1".to_string(), "shell".to_string(), tx)
            .await;
        let (guard, tool) = state.take_result_tx("call-1").await.expect("present");
        assert_eq!(tool, "shell");
        drop(guard);
        let frame = rx.recv().await.expect("a synthetic terminal must arrive");
        match frame.frame {
            Some(Frame::Complete(c)) => assert_eq!(c.outcome, ToolOutcome::Failed as i32),
            other => panic!("expected a FAILED terminal, got {other:?}"),
        }
    }

    /// begin_call env-keys the queued call's args: a git-style tool with an
    /// arg `message` mapped to env `MESSAGE` enqueues `{"MESSAGE": "hi"}`, not
    /// the raw request key. With no kube client the call attaches to a seeded
    /// active job rather than spawning.
    #[tokio::test]
    async fn begin_call_enqueues_env_keyed_args() {
        let mut config = HashMap::new();
        config.insert(
            "git".to_string(),
            ToolsetEntry {
                image: Some("git:local".to_string()),
                ..Default::default()
            },
        );
        let state = DispatchState::new(
            None,
            "ns".to_string(),
            "http://harness:9090".to_string(),
            "ws".to_string(),
            SchedulingConfig::default(),
            ToolsetConfig::from_map(config),
        );
        state
            .set_active_job(active("git-push", "job-x", None))
            .await;

        let mut args = HashMap::new();
        args.insert("MESSAGE".to_string(), "hi".to_string());
        let (_call_id, _rx) = state
            .begin_call("git-push", "git", args, None)
            .await
            .expect("attach to the seeded job");

        let call = state
            .dequeue_call("git-push", "job-x")
            .await
            .expect("the seeded pod claims its call");
        assert_eq!(call.args.get("MESSAGE").map(String::as_str), Some("hi"));
        assert!(
            !call.args.contains_key("message"),
            "the queued args must be env-keyed, never the raw request key"
        );
    }

    /// The warm local model's base URL is a pure function of the model key, the
    /// namespace, and the Service's serving port — the `inference-<key>`
    /// ClusterIP record the harness dials directly, no job and no apiserver
    /// read. Mirrors the per-pod dial-name builder for the no-job path.
    #[test]
    fn local_service_base_url_is_the_inference_key_cluster_ip_record() {
        assert_eq!(
            local_service_base_url("in-cluster", "tenant-x", 8080),
            "http://inference-in-cluster.tenant-x.svc.cluster.local:8080/v1",
        );
    }

    /// The local base URL plus the OpenAI provider's `/chat/completions` leaf must
    /// land on the exact path the inference fence allowlists. The `/v1` segment
    /// lives in the builder because the chart drops it when it parses the port,
    /// so this is the only assertion that exercises the path the harness dials.
    #[test]
    fn local_base_url_plus_provider_leaf_matches_the_fence_allowlisted_path() {
        let request_url = format!(
            "{}/chat/completions",
            local_service_base_url("local", "e2e-test", 8080)
        );
        assert_eq!(
            request_url,
            "http://inference-local.e2e-test.svc.cluster.local:8080/v1/chat/completions",
        );
    }

    /// The provider format string resolves through the same serde spelling the
    /// model config uses; an unrecognized format is refused, not defaulted.
    #[test]
    fn parse_format_reads_the_configured_spellings() {
        assert_eq!(parse_format("openai").unwrap(), Format::OpenAi);
        assert_eq!(parse_format("anthropic").unwrap(), Format::Anthropic);
        assert_eq!(parse_format("gemini").unwrap(), Format::Gemini);
        assert!(parse_format("not-a-format").is_err());
    }

    /// A turn whose model is absent fails closed: the source yields one
    /// `TurnError` and ends, so the harness never invents a default model.
    #[tokio::test]
    async fn a_turn_with_no_model_fails_closed() {
        let mut dispatch = InferenceDispatch::new(
            ModelConfigs::default(),
            None,
            "ns".to_string(),
            "capability-ws".to_string(),
            "ws".to_string(),
            SchedulingConfig::default(),
        );
        let request = TurnRequest {
            model: None,
            conversation_id: "c".to_string(),
            ..Default::default()
        };
        let mut source = dispatch.turn(request).await.expect("turn opens a source");
        let first = source.next_event().await.expect("one error frame");
        assert!(matches!(
            first.unwrap().event,
            Some(turn_event::Event::Error(_))
        ));
        assert!(
            source.next_event().await.is_none(),
            "the fail-closed source ends after the single error frame"
        );
    }

    /// A turn naming a model the catalog does not hold fails closed the same
    /// way: no fallback, one error frame, then end.
    #[tokio::test]
    async fn a_turn_with_an_unknown_model_fails_closed() {
        let mut dispatch = InferenceDispatch::new(
            ModelConfigs::default(),
            None,
            "ns".to_string(),
            "capability-ws".to_string(),
            "ws".to_string(),
            SchedulingConfig::default(),
        );
        let request = TurnRequest {
            model: Some("no-such-model".to_string()),
            conversation_id: "c".to_string(),
            ..Default::default()
        };
        let mut source = dispatch.turn(request).await.expect("turn opens a source");
        let first = source.next_event().await.expect("one error frame");
        assert!(matches!(
            first.unwrap().event,
            Some(turn_event::Event::Error(_))
        ));
        assert!(source.next_event().await.is_none());
    }

    /// A fake provider whose `call` yields a fixed event script. Drives
    /// [`ProviderTurnSource`] end to end so the open-guard and the terminal
    /// stop-reason mapping run against a real stream.
    struct FakeProvider {
        events: Vec<StreamEvent>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for FakeProvider {
        async fn call(
            &self,
            _messages: &[proto_common::Message],
            _system: Option<&str>,
            _tools: &[proto_common::ToolDefinition],
            _params: Option<&serde_json::Map<String, serde_json::Value>>,
            _config: &ProviderConfig,
        ) -> Result<
            Pin<Box<dyn tokio_stream::Stream<Item = Result<StreamEvent, String>> + Send>>,
            String,
        > {
            let events = self.events.clone();
            Ok(Box::pin(tokio_stream::iter(
                events.into_iter().map(Ok::<StreamEvent, String>),
            )))
        }

        fn managed_fields(&self) -> &'static [&'static str] {
            &[]
        }
    }

    fn provider_source(stop_reason: &str) -> ProviderTurnSource {
        let provider = Box::new(FakeProvider {
            events: vec![
                StreamEvent::ContentDelta {
                    text: "hi".to_string(),
                },
                StreamEvent::Done {
                    stop_reason: stop_reason.to_string(),
                },
            ],
        });
        let config = ProviderConfig {
            model: "m".to_string(),
            api_key: String::new(),
        };
        let request = TurnRequest {
            conversation_id: "c".to_string(),
            ..Default::default()
        };
        ProviderTurnSource::new(provider, config, request)
    }

    /// A `tool_use` terminal signal maps to `StopReason::ToolUse` on the
    /// assembled `TurnComplete`, never collapsed to `EndTurn`. Driving the
    /// source also forces the lazy provider open on the first `next_event`, so
    /// the content delta must arrive before the completion. Breaks if the
    /// `"tool_use"` arm of `assemble_turn_complete` is dropped (the default arm
    /// would map it to `EndTurn`), and breaks if the open-guard is inverted (the
    /// stream is never opened and the first `next_event` panics).
    #[tokio::test]
    async fn provider_turn_source_maps_tool_use_stop_reason() {
        let mut source = provider_source("tool_use");
        let first = source
            .next_event()
            .await
            .expect("the streamed content delta")
            .expect("a forwarded frame");
        assert!(
            matches!(first.event, Some(turn_event::Event::ContentDelta(_))),
            "the provider stream must open and forward the content delta first"
        );
        let second = source
            .next_event()
            .await
            .expect("the assembled completion")
            .expect("a completion frame");
        match second.event {
            Some(turn_event::Event::Complete(tc)) => assert_eq!(
                tc.stop_reason,
                proto_common::StopReason::ToolUse as i32,
                "a tool_use terminal must map to ToolUse, not the EndTurn default"
            ),
            other => panic!("expected a TurnComplete, got {other:?}"),
        }
    }

    /// A `max_tokens` terminal signal maps to `StopReason::MaxTokens`, never
    /// collapsed to `EndTurn`. Breaks if the `"max_tokens"` arm of
    /// `assemble_turn_complete` is dropped, and breaks if the open-guard is
    /// inverted so the first `next_event` never opens the provider stream.
    #[tokio::test]
    async fn provider_turn_source_maps_max_tokens_stop_reason() {
        let mut source = provider_source("max_tokens");
        let first = source
            .next_event()
            .await
            .expect("the streamed content delta")
            .expect("a forwarded frame");
        assert!(
            matches!(first.event, Some(turn_event::Event::ContentDelta(_))),
            "the provider stream must open and forward the content delta first"
        );
        let second = source
            .next_event()
            .await
            .expect("the assembled completion")
            .expect("a completion frame");
        match second.event {
            Some(turn_event::Event::Complete(tc)) => assert_eq!(
                tc.stop_reason,
                proto_common::StopReason::MaxTokens as i32,
                "a max_tokens terminal must map to MaxTokens, not the EndTurn default"
            ),
            other => panic!("expected a TurnComplete, got {other:?}"),
        }
    }
}

/// The harness is the CLIENT of the reversed tool-dispatch protocol. It dials
/// the serving pod by its per-pod headless DNS name, sends the assignment as the
/// first `RunToolCommand` on the held `ToolJob.Run` stream, forwards the pod's
/// result frames onto the call's channel, and pushes cancel on that same
/// stream's outbound half. When the per-pod record never resolves it retries for
/// the readiness window and then fails the call with the synthetic terminal.
///
/// These tests target the confirmed `ToolJob` proto and two harness seams the
/// dial-hold-retry rewrite introduces:
///   - `pod_dial_name(call_id, service, namespace) -> String` — a pure builder,
///     no apiserver call;
///   - `dial_and_run(endpoint, assignment, result_tx, cancel, ready_window)` —
///     the per-call client task.
#[cfg(test)]
mod dial_client_tests {
    use std::net::TcpListener;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use proto_common::tool_result_frame::Frame;
    use proto_common::{ToolComplete, ToolOutcome, ToolResultFrame};
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_stream::{Stream, StreamExt};
    use tokio_util::sync::CancellationToken;
    use tonic::transport::Server;
    use tonic::{Request, Response, Status, Streaming};

    use toolset_proto::run_tool_command::Command;
    use toolset_proto::tool_job_server::{ToolJob, ToolJobServer};
    use toolset_proto::{RunToolCommand, ToolCallAssignment};

    use super::{dial_and_run, pod_dial_name, RESULT_CHANNEL_CAPACITY};

    /// A stand-in serving pod. Records every inbound `RunToolCommand` and, once
    /// its trigger fires, streams `frames` back on the response half. With
    /// `wait_for_cancel` it withholds the frames until it observes a cancel on
    /// the request half; otherwise it streams them right after the assignment.
    struct FakePod {
        inbound: Arc<Mutex<Vec<RunToolCommand>>>,
        frames: Vec<ToolResultFrame>,
        wait_for_cancel: bool,
    }

    type FrameStream = Pin<Box<dyn Stream<Item = Result<ToolResultFrame, Status>> + Send>>;

    #[tonic::async_trait]
    impl ToolJob for FakePod {
        type RunStream = FrameStream;

        async fn run(
            &self,
            request: Request<Streaming<RunToolCommand>>,
        ) -> Result<Response<Self::RunStream>, Status> {
            let mut inbound = request.into_inner();
            let log = self.inbound.clone();
            let frames = self.frames.clone();
            let wait_for_cancel = self.wait_for_cancel;
            let (tx, rx) = mpsc::channel::<Result<ToolResultFrame, Status>>(16);
            tokio::spawn(async move {
                let mut first = true;
                while let Some(msg) = inbound.next().await {
                    let Ok(cmd) = msg else { break };
                    let is_cancel = matches!(cmd.command, Some(Command::Cancel(_)));
                    log.lock().unwrap().push(cmd);
                    let send_now = if wait_for_cancel { is_cancel } else { first };
                    first = false;
                    if send_now {
                        for f in &frames {
                            let _ = tx.send(Ok(f.clone())).await;
                        }
                        break;
                    }
                }
            });
            Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
        }
    }

    /// Bind a fake pod on an ephemeral port and return its dial endpoint.
    fn serve(pod: FakePod) -> String {
        let reserve = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = reserve.local_addr().unwrap();
        drop(reserve);
        tokio::spawn(async move {
            Server::builder()
                .add_service(ToolJobServer::new(pod))
                .serve(addr)
                .await
                .unwrap();
        });
        format!("http://{addr}")
    }

    fn stdout_frame(s: &str) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Stdout(s.to_string())),
        }
    }

    fn terminal(outcome: ToolOutcome) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Complete(ToolComplete {
                outcome: outcome as i32,
                exit_code: 0,
            })),
        }
    }

    fn assignment(call_id: &str) -> ToolCallAssignment {
        ToolCallAssignment {
            call_id: call_id.to_string(),
            working_dir: "/workspace".to_string(),
            args: Default::default(),
        }
    }

    async fn wait_until(cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not met within 5s");
    }

    /// The pod's dial name is a pure function of the call id, the
    /// workspace headless Service, and the namespace — computed with no
    /// apiserver call. Breaks if the builder reads `status.podIP` or reshapes
    /// the per-pod record.
    #[test]
    fn dial_name_is_the_per_pod_headless_record() {
        assert_eq!(
            pod_dial_name("call-1", "capability-alpha", "tenant-x"),
            "call-1.capability-alpha.tenant-x.svc.cluster.local",
        );
    }

    /// The harness sends the tool-call assignment as the FIRST message on
    /// the stream it opens. Breaks if the client sends anything before the
    /// assignment, or never sends it.
    #[tokio::test]
    async fn the_assignment_is_the_first_message_on_the_stream() {
        let inbound = Arc::new(Mutex::new(Vec::new()));
        let endpoint = serve(FakePod {
            inbound: inbound.clone(),
            frames: vec![terminal(ToolOutcome::Done)],
            wait_for_cancel: false,
        });

        let (tx, _rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        dial_and_run(
            endpoint,
            assignment("call-abc"),
            tx,
            CancellationToken::new(),
            Duration::from_secs(5),
        )
        .await;

        let log = inbound.lock().unwrap();
        match log.first().and_then(|c| c.command.as_ref()) {
            Some(Command::Assignment(a)) => assert_eq!(a.call_id, "call-abc"),
            other => panic!("first message must be the assignment, got {other:?}"),
        }
    }

    /// The frames the pod streams arrive on the very connection the harness
    /// opened, in order, terminal last. Breaks if the client drops inbound
    /// frames (the receiver would see only the guard's synthetic terminal).
    #[tokio::test]
    async fn frames_stream_back_on_the_opened_connection() {
        let endpoint = serve(FakePod {
            inbound: Arc::new(Mutex::new(Vec::new())),
            frames: vec![stdout_frame("hello-from-pod"), terminal(ToolOutcome::Done)],
            wait_for_cancel: false,
        });

        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        dial_and_run(
            endpoint,
            assignment("c"),
            tx,
            CancellationToken::new(),
            Duration::from_secs(5),
        )
        .await;

        let first = rx.recv().await.expect("a forwarded frame");
        assert!(
            matches!(first.frame, Some(Frame::Stdout(ref s)) if s == "hello-from-pod"),
            "the pod's output frame must reach the call channel unchanged"
        );
        let last = rx.recv().await.expect("terminal");
        assert!(
            matches!(last.frame, Some(Frame::Complete(ref c)) if c.outcome == ToolOutcome::Done as i32),
            "the pod's terminal must reach the call channel"
        );
    }

    /// Cancelling the call token makes the harness deliver a
    /// `RunToolCommand{cancel}` on the SAME stream's outbound half — not a
    /// separate pod-initiated call. Breaks if cancel is dropped or routed off
    /// the held stream.
    #[tokio::test]
    async fn cancel_travels_the_outbound_half_of_the_held_stream() {
        let inbound = Arc::new(Mutex::new(Vec::new()));
        let endpoint = serve(FakePod {
            inbound: inbound.clone(),
            frames: vec![terminal(ToolOutcome::Done)],
            wait_for_cancel: true,
        });

        let cancel = CancellationToken::new();
        let (tx, _rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        let driver = tokio::spawn(dial_and_run(
            endpoint,
            assignment("c"),
            tx,
            cancel.clone(),
            Duration::from_secs(5),
        ));

        // The assignment lands first, then the fired cancel rides the same stream.
        wait_until(|| !inbound.lock().unwrap().is_empty()).await;
        cancel.cancel();
        wait_until(|| {
            inbound
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c.command, Some(Command::Cancel(_))))
        })
        .await;
        let _ = driver.await;

        let log = inbound.lock().unwrap();
        assert!(
            matches!(
                log.first().and_then(|c| c.command.as_ref()),
                Some(Command::Assignment(_))
            ),
            "the assignment must precede the cancel on the same stream"
        );
        assert!(
            log.iter()
                .any(|c| matches!(c.command, Some(Command::Cancel(_)))),
            "the harness must deliver cancel on the held stream, not a new call"
        );
    }

    /// When the per-pod record never resolves, the harness retries the dial
    /// for its readiness window and then fails the call with the synthetic
    /// FAILED terminal — the same terminal the ready deadline fails a call with
    /// today. Breaks if the client fails fast without waiting the window, or
    /// fails silently without the terminal.
    #[tokio::test]
    async fn an_unresolvable_pod_fails_the_call_after_the_ready_window() {
        // A reserved-then-freed port: nothing listens, so every dial refuses.
        let reserve = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = reserve.local_addr().unwrap();
        drop(reserve);

        let window = Duration::from_millis(400);
        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        let started = Instant::now();
        dial_and_run(
            format!("http://{dead}"),
            assignment("c"),
            tx,
            CancellationToken::new(),
            window,
        )
        .await;

        let frame = rx.recv().await.expect("a terminal must arrive");
        assert!(
            matches!(frame.frame, Some(Frame::Complete(ref c)) if c.outcome == ToolOutcome::Failed as i32),
            "an unreachable pod fails the call with a FAILED terminal"
        );
        assert!(
            started.elapsed() >= window - Duration::from_millis(50),
            "the harness must retry for the full readiness window before failing"
        );
    }

    /// A pod that connects, streams a non-terminal frame, then closes the stream
    /// WITHOUT a terminal `Complete` (a mid-call crash / OOM) still fails the call
    /// with the synthetic FAILED terminal, so the parked drain never awaits
    /// forever. Breaks if `dial_and_run` marks the guard complete when no terminal
    /// arrived (dropping the `if saw_terminal` guard): the guard would suppress the
    /// synthetic FAILED and only the pod's partial frame would reach the channel.
    #[tokio::test]
    async fn stream_ending_without_terminal_yields_synthetic_failed() {
        let endpoint = serve(FakePod {
            inbound: Arc::new(Mutex::new(Vec::new())),
            frames: vec![stdout_frame("partial-output")],
            wait_for_cancel: false,
        });

        let (tx, mut rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        let saw_terminal = dial_and_run(
            endpoint,
            assignment("c"),
            tx,
            CancellationToken::new(),
            Duration::from_secs(5),
        )
        .await;

        // The live connection streams one non-terminal frame first, proving the
        // call reached the stream drain rather than the dial-timeout branch.
        let first = rx.recv().await.expect("the pod's partial frame");
        assert!(
            matches!(first.frame, Some(Frame::Stdout(ref s)) if s == "partial-output"),
            "the pod's streamed frame must reach the call channel"
        );
        // The stream then closed with no terminal Complete, so the dropped guard
        // synthesizes the FAILED terminal that unblocks the parked drain.
        let last = rx.recv().await.expect("a synthetic terminal must arrive");
        match last.frame {
            Some(Frame::Complete(c)) => assert_eq!(c.outcome, ToolOutcome::Failed as i32),
            other => panic!("expected a synthetic FAILED terminal, got {other:?}"),
        }
        assert!(
            !saw_terminal,
            "no terminal Complete streamed, so dial_and_run must report false"
        );
    }
}

/// The inference twin of [`dial_client_tests`]: the harness dials a per-call
/// inference-runtime pod by its per-pod DNS name, sends the `TurnAssignment` as
/// the first `RunInferenceCommand` on the held `InferenceJob.Run` stream, and
/// reads the model's `TurnEvent`s straight off the response half.
///
/// These target `dial_inference(endpoint, assignment, ready_window)` — the
/// per-call inference client — against a real `InferenceJob` server on loopback.
#[cfg(test)]
mod dial_inference_tests {
    use std::net::TcpListener;
    use std::pin::Pin;
    use std::time::{Duration, Instant};

    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_stream::{Stream, StreamExt};
    use tonic::transport::Server;
    use tonic::{Request, Response, Status, Streaming};

    use toolset_proto::inference_job_server::{InferenceJob, InferenceJobServer};
    use toolset_proto::{ContentDelta, RunInferenceCommand, TurnAssignment, TurnEvent};

    use super::{dial_inference, turn_event, TurnSource};

    /// A stand-in inference pod. Reads the first inbound command (the
    /// assignment), then streams its scripted `TurnEvent`s back on the response
    /// half — the same held connection.
    struct FakeInferencePod {
        events: Vec<TurnEvent>,
    }

    type FrameStream = Pin<Box<dyn Stream<Item = Result<TurnEvent, Status>> + Send>>;

    #[tonic::async_trait]
    impl InferenceJob for FakeInferencePod {
        type RunStream = FrameStream;

        async fn run(
            &self,
            request: Request<Streaming<RunInferenceCommand>>,
        ) -> Result<Response<Self::RunStream>, Status> {
            let mut inbound = request.into_inner();
            let events = self.events.clone();
            let (tx, rx) = mpsc::channel::<Result<TurnEvent, Status>>(16);
            tokio::spawn(async move {
                // Stream the events only after the assignment lands on the
                // inbound half, matching the pod's assignment-first contract.
                if inbound.next().await.is_some() {
                    for e in events {
                        let _ = tx.send(Ok(e)).await;
                    }
                }
            });
            Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
        }
    }

    fn serve(pod: FakeInferencePod) -> String {
        let reserve = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = reserve.local_addr().unwrap();
        drop(reserve);
        tokio::spawn(async move {
            Server::builder()
                .add_service(InferenceJobServer::new(pod))
                .serve(addr)
                .await
                .unwrap();
        });
        format!("http://{addr}")
    }

    fn assignment() -> TurnAssignment {
        TurnAssignment {
            conversation_id: "c".to_string(),
            ..Default::default()
        }
    }

    fn content_event(text: &str) -> TurnEvent {
        TurnEvent {
            event: Some(turn_event::Event::ContentDelta(ContentDelta {
                text: text.to_string(),
            })),
        }
    }

    /// A successful dial returns the pod's response stream, and the model events
    /// the pod streams arrive on it in order. Breaks if the assignment is not
    /// sent first (the pod withholds its events) or the response half is not
    /// wired through to the returned source.
    #[tokio::test]
    async fn dial_inference_returns_the_pods_event_stream() {
        let endpoint = serve(FakeInferencePod {
            events: vec![content_event("served"), content_event("more")],
        });

        let mut source = dial_inference(endpoint, assignment(), Duration::from_secs(5))
            .await
            .expect("a reachable pod returns its stream");

        let first = source
            .next_event()
            .await
            .expect("a streamed event")
            .expect("an ok frame");
        assert!(
            matches!(
                first.event,
                Some(turn_event::Event::ContentDelta(ref d)) if d.text == "served"
            ),
            "the pod's first model event must reach the returned source unchanged"
        );
    }

    /// When no pod answers, `dial_inference` retries for the full readiness
    /// window and then returns `Err` — it does not fail fast and does not hang.
    /// Breaks if the deadline is computed as `now - window` (the dial returns
    /// immediately, well under the window) or the expiry check is inverted from
    /// `now >= deadline` to `now < deadline` (also an immediate return).
    #[tokio::test]
    async fn dial_inference_fails_after_the_ready_window_when_no_pod_answers() {
        // A reserved-then-freed port: nothing listens, so every dial refuses.
        let reserve = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = reserve.local_addr().unwrap();
        drop(reserve);

        let window = Duration::from_millis(400);
        let started = Instant::now();
        let result = dial_inference(format!("http://{dead}"), assignment(), window).await;

        assert!(
            result.is_err(),
            "an unreachable pod must return an error, not a stream or a hang"
        );
        assert!(
            started.elapsed() >= window - Duration::from_millis(50),
            "dial_inference must retry for the full readiness window before failing"
        );
    }
}
