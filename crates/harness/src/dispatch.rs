//! In-process tool-call dispatch, relocated from the toolset controller into
//! the per-workspace harness.
//!
//! The harness both PRODUCES calls — the agent turn's `Source::Toolset` arm
//! drives [`DispatchState::begin_call`] — and SERVES the tool-job pods that run
//! them, via the pod-facing `GetToolCall` / `StreamToolResult` /
//! `AwaitToolCancel` surface ([`ToolDispatchService`]). One workspace per
//! harness, so the credential-containment isolation the controller enforced
//! across workspaces reduces here to per-grant isolation: `(tool, grant)` keys
//! keep two grants' Jobs and queued calls for one tool in separate slots.
//!
//! Capability-job ingress is restricted to the harness by NetworkPolicy, so the
//! pod-facing surface authenticates structurally rather than by token, matching
//! the harness's existing gRPC server.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use proto_common::tool_result_frame::Frame;
use proto_common::{
    AwaitToolResultRequest, CallToolRequest, CancelTurnRequest, CancelTurnResponse, ToolComplete,
    ToolOutcome, ToolResultFrame, WatchToolsRequest,
};
use shared::scheduling::SchedulingConfig;
use shared::toolset::{CapabilityGrant, ToolsetConfig, WORKSPACE_MOUNT_PATH};
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};
use uuid::Uuid;

use toolset_proto::toolset_controller_server::ToolsetController;
use toolset_proto::{
    AwaitToolCancelRequest, AwaitTurnCancelRequest, CancelToolCallRequest, CancelToolCallResponse,
    GetToolCallRequest, GetTurnRequest, ReportDiscoveredToolsAck, ReportDiscoveredToolsRequest,
    SendToolResultAck, ToolCallAssignment, ToolCallHandle, ToolCancelSignal, ToolList, TurnAck,
    TurnAssignment, TurnCancelSignal, TurnEvent, TurnRequest, TurnResultChunk,
};

use crate::job;

/// Bound on a tool call's in-flight frame channel. The tool job client-streams
/// its output into it; the producer's drain empties it.
pub(crate) const RESULT_CHANNEL_CAPACITY: usize = 64;

/// Bound on the wait for a spawned or attaching Job to ask for its work. Running
/// work carries no time bound.
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
    dispatch_addr: String,
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
        dispatch_addr: String,
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
                dispatch_addr,
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

    async fn get_active_job_by_id(&self, tool_name: &str, job_id: &str) -> Option<ActiveJob> {
        if job_id.is_empty() {
            return None;
        }
        let jobs = self.active_jobs.read().await;
        for (key, job) in jobs.iter() {
            if key.0 == tool_name && job.job_id == job_id {
                return Some(job.clone());
            }
        }
        None
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
                        &self.spawn.dispatch_addr,
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
            self.arm_ready_deadline(
                tool_name.to_string(),
                call_grant,
                call_id.clone(),
                target_job_name,
            );
        }

        Ok((call_id, result_rx))
    }

    /// Bound this call's wait for its Job to ask for work. On expiry the call is
    /// failed with the terminal the drain understands, its bookkeeping dropped,
    /// and its stale active-job record evicted so the next call spawns fresh
    /// rather than re-attaching to a pod that never connected. The harness holds
    /// no delete RBAC, so it never deletes the Job — its own
    /// `ttlSecondsAfterFinished` reaps it.
    fn arm_ready_deadline(
        self: &Arc<Self>,
        tool_name: String,
        grant: Option<String>,
        call_id: String,
        job_name: String,
    ) {
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        let state = self.clone();
        tokio::spawn(async move {
            let job_name = Some(job_name).filter(|n| !n.is_empty());

            tokio::time::sleep_until(deadline).await;
            if !state
                .remove_pending_call(&tool_name, grant.as_deref(), &call_id)
                .await
            {
                return;
            }
            warn!(
                call_id = %call_id,
                tool = %tool_name,
                job = job_name.as_deref().unwrap_or("<none>"),
                "tool Job did not ask for work within readyTimeout; failing the call"
            );

            state.finish_call(&call_id).await;
            drop(state.take_result_tx(&call_id).await);

            if let Some(job_name) = job_name {
                if state.remove_active_job_named(&tool_name, &job_name).await
                    == RecordEviction::Removed
                {
                    state.retire_calls_for_tool_job(&tool_name).await;
                }
            }
        });
    }

    /// Forward a pod's inbound frame stream to the call's parked drain, then
    /// retire the call.
    pub(crate) async fn forward_result_frames<S>(
        &self,
        call_id: String,
        mut stream: S,
    ) -> Result<Response<SendToolResultAck>, Status>
    where
        S: Stream<Item = Result<ToolResultFrame, Status>> + Unpin,
    {
        let (mut guard, tool_name) = self.take_result_tx(&call_id).await.ok_or_else(|| {
            Status::not_found(format!("no pending result for call_id: {call_id}"))
        })?;

        info!(call_id = %call_id, "receiving tool result stream");

        let mut saw_terminal = false;
        while let Some(frame) = stream.next().await {
            let frame = frame.map_err(|e| Status::internal(format!("frame stream error: {e}")))?;
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

        Ok(Response::new(SendToolResultAck {}))
    }
}

/// Pod-facing gRPC surface: the tool-job pod pulls its assignment, streams its
/// result back, and long-polls for a cancel. Ingress is netpol-restricted to
/// capability-job pods, so the surface authenticates structurally. Every other
/// `ToolsetController` method is served by the toolset controller, not the
/// harness, and is refused here.
pub(crate) struct ToolDispatchService {
    state: Arc<DispatchState>,
}

impl ToolDispatchService {
    pub(crate) fn new(state: Arc<DispatchState>) -> Self {
        Self { state }
    }
}

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl ToolsetController for ToolDispatchService {
    type TurnStream = BoxStream<TurnEvent>;
    type WatchToolsStream = BoxStream<ToolList>;
    type AwaitToolResultStream = BoxStream<ToolResultFrame>;

    async fn get_tool_call(
        &self,
        request: Request<GetToolCallRequest>,
    ) -> Result<Response<ToolCallAssignment>, Status> {
        let req = request.into_inner();
        let tool_name = &req.tool_name;

        loop {
            let waiter = self.state.call_waiter();
            tokio::pin!(waiter);
            waiter.as_mut().enable();

            // Every pass: a pod outlives the retirement of its own Job, and this
            // refusal is what shuts it down.
            if self
                .state
                .get_active_job_by_id(tool_name, &req.job_id)
                .await
                .is_none()
            {
                warn!(
                    job_id = %req.job_id,
                    tool = %tool_name,
                    "refusing GetToolCall: this job is not the active job for its tool"
                );
                return Err(Status::failed_precondition(format!(
                    "job_id {} does not match an active job for tool {tool_name}",
                    req.job_id
                )));
            }

            if let Some(call) = self.state.dequeue_call(tool_name, &req.job_id).await {
                info!(call_id = %call.call_id, job_id = %req.job_id, tool = %tool_name, "dispatching call to runtime");
                return Ok(Response::new(ToolCallAssignment {
                    call_id: call.call_id,
                    working_dir: call.working_dir,
                    args: call.args,
                }));
            }

            waiter.await;
        }
    }

    async fn stream_tool_result(
        &self,
        request: Request<Streaming<ToolResultFrame>>,
    ) -> Result<Response<SendToolResultAck>, Status> {
        let call_id = request
            .metadata()
            .get("x-toolset-call-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| Status::invalid_argument("missing x-toolset-call-id metadata header"))?;
        let stream = request.into_inner();
        self.state.forward_result_frames(call_id, stream).await
    }

    async fn await_tool_cancel(
        &self,
        request: Request<AwaitToolCancelRequest>,
    ) -> Result<Response<ToolCancelSignal>, Status> {
        let call_id = request.into_inner().call_id;
        if let Some(token) = self.state.call_cancel_token(&call_id).await {
            token.cancelled().await;
        }
        Ok(Response::new(ToolCancelSignal {}))
    }

    // ---- Served by the toolset controller, refused here ----

    async fn turn(&self, _: Request<TurnRequest>) -> Result<Response<Self::TurnStream>, Status> {
        Err(unsupported("Turn"))
    }

    async fn cancel_turn(
        &self,
        _: Request<CancelTurnRequest>,
    ) -> Result<Response<CancelTurnResponse>, Status> {
        Err(unsupported("CancelTurn"))
    }

    async fn watch_tools(
        &self,
        _: Request<WatchToolsRequest>,
    ) -> Result<Response<Self::WatchToolsStream>, Status> {
        Err(unsupported("WatchTools"))
    }

    async fn begin_tool_call(
        &self,
        _: Request<CallToolRequest>,
    ) -> Result<Response<ToolCallHandle>, Status> {
        Err(unsupported("BeginToolCall"))
    }

    async fn await_tool_result(
        &self,
        _: Request<AwaitToolResultRequest>,
    ) -> Result<Response<Self::AwaitToolResultStream>, Status> {
        Err(unsupported("AwaitToolResult"))
    }

    async fn cancel_tool_call(
        &self,
        _: Request<CancelToolCallRequest>,
    ) -> Result<Response<CancelToolCallResponse>, Status> {
        Err(unsupported("CancelToolCall"))
    }

    async fn get_turn(
        &self,
        _: Request<GetTurnRequest>,
    ) -> Result<Response<TurnAssignment>, Status> {
        Err(unsupported("GetTurn"))
    }

    async fn stream_turn_result(
        &self,
        _: Request<Streaming<TurnResultChunk>>,
    ) -> Result<Response<TurnAck>, Status> {
        Err(unsupported("StreamTurnResult"))
    }

    async fn await_turn_cancel(
        &self,
        _: Request<AwaitTurnCancelRequest>,
    ) -> Result<Response<TurnCancelSignal>, Status> {
        Err(unsupported("AwaitTurnCancel"))
    }

    async fn report_discovered_tools(
        &self,
        _: Request<ReportDiscoveredToolsRequest>,
    ) -> Result<Response<ReportDiscoveredToolsAck>, Status> {
        Err(unsupported("ReportDiscoveredTools"))
    }
}

fn unsupported(rpc: &str) -> Status {
    Status::unimplemented(format!(
        "{rpc} is served by the toolset controller, not the harness dispatch surface"
    ))
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
}
