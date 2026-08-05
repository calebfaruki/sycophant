use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use proto_common::tool_result_frame::Frame;
use proto_common::{ToolComplete, ToolOutcome, ToolResultFrame};
use shared::scheduling::SchedulingConfig;
use tokio::sync::{mpsc, watch, Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::crd::{ModelSpec, ProviderSpec, Toolset};
use crate::registry::ArgDecl;
use toolset_proto::{turn_result_chunk, TurnAssignment, TurnError, TurnResultChunk, TurnRole};

/// Bound on a tool call's in-flight frame channel, and on a turn's result
/// chunk channel. The worker client-streams its output into it; the harness's
/// stream drains it.
pub const RESULT_CHANNEL_CAPACITY: usize = 64;

// =========================================================================
// Tool dispatch: toolset bindings, tool registry, pending calls, active Jobs
// =========================================================================

#[derive(Clone)]
pub struct WorkspaceBindings {
    map: HashMap<String, Vec<String>>,
}

impl WorkspaceBindings {
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read bindings file {path}: {e}"))?;
        let map: HashMap<String, Vec<String>> = serde_yaml::from_str(&content)
            .map_err(|e| format!("failed to parse bindings YAML: {e}"))?;
        Ok(Self { map })
    }

    pub fn empty() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn from_map(map: HashMap<String, Vec<String>>) -> Self {
        Self { map }
    }

    pub fn toolsets_for(&self, workspace: &str) -> &[String] {
        self.map.get(workspace).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn has_toolset(&self, workspace: &str, toolset: &str) -> bool {
        self.toolsets_for(workspace).iter().any(|c| c == toolset)
    }

    /// Workspaces bound to `toolset`, in a stable order. The discovery Job runs
    /// under one such workspace's ServiceAccount so its projected token is
    /// mintable; the report it sends is workspace-independent.
    pub fn workspaces_for_toolset(&self, toolset: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .map
            .iter()
            .filter(|(_, toolsets)| toolsets.iter().any(|t| t == toolset))
            .map(|(ws, _)| ws.clone())
            .collect();
        out.sort();
        out
    }
}

impl Default for WorkspaceBindings {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Clone)]
pub struct RegisteredTool {
    pub name: String,
    pub toolset_name: String,
    pub description: String,
    pub args: Vec<ArgDecl>,
}

/// RAII wrapper around a pending tool call's frame sender. Guarantees the
/// `AwaitToolResult` stream always terminates: the runtime forwards its frames
/// through `sender()` and, on the terminal `ToolComplete`, `mark_complete` is
/// called so `Drop` is silent. If the sender is dropped WITHOUT a terminal
/// having been forwarded — a tool-worker Job reaped or vanished mid-stream —
/// `Drop` `try_send`s a synthetic error terminal, so a harness parked on the
/// stream unblocks instead of awaiting forever.
pub struct ToolResultGuard {
    tx: mpsc::Sender<ToolResultFrame>,
    complete: bool,
}

impl ToolResultGuard {
    pub fn new(tx: mpsc::Sender<ToolResultFrame>) -> Self {
        Self {
            tx,
            complete: false,
        }
    }

    /// The frame sender the result-forwarding handler pushes frames through.
    pub fn sender(&self) -> &mpsc::Sender<ToolResultFrame> {
        &self.tx
    }

    /// Mark that a terminal frame was forwarded, so `Drop` does not emit a
    /// second (synthetic) terminal.
    pub fn mark_complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for ToolResultGuard {
    fn drop(&mut self) {
        if !self.complete {
            // Synthetic backup terminal: the runtime delivered no terminal (a
            // vanished pod or a keepalive reap), which is never a user cancel,
            // so it is FAILED — never CANCELED.
            let _ = self.tx.try_send(ToolResultFrame {
                frame: Some(Frame::Complete(ToolComplete {
                    outcome: ToolOutcome::Failed as i32,
                    exit_code: -1,
                })),
            });
        }
    }
}

pub struct PendingCall {
    pub call_id: String,
    pub tool_name: String,
    pub workspace: String,
    pub args: HashMap<String, String>,
    pub working_dir: String,
}

#[derive(Clone)]
pub struct ActiveJob {
    pub job_name: String,
    pub tool_name: String,
    pub workspace: String,
    pub last_activity: Instant,
    pub keepalive_seconds: u64,
}

// =========================================================================
// Turn dispatch: model slots, pending/active turns, provider registry
// =========================================================================

pub struct PendingTurn {
    pub assignment: TurnAssignment,
    pub result_tx: mpsc::Sender<TurnResultChunk>,
    pub workspace: String,
    pub conversation_id: String,
    pub reply_channel: Option<String>,
    pub role: Option<TurnRole>,
    pub correlation_id: Option<String>,
    /// System prompt the prompt worker will receive for this turn.
    pub system_prompt: Option<String>,
}

pub struct JobCreateSpec {
    pub model: ModelSpec,
    pub provider: ProviderSpec,
}

pub enum JobAction {
    AlreadyConnected,
    NoKubeClient,
    NoModelSpec,
    NoProviderSpec(String),
    Create(Box<JobCreateSpec>),
}

/// Outcome of [`ControllerState::take_active_turn_if_owned`].
#[derive(Debug)]
pub enum TakeTurnError {
    /// No active turn loaded for this model slot.
    NoActiveTurn,
    /// Active turn exists but the caller's workspace does not own it.
    /// The slot is left intact for the legitimate owner.
    OwnerMismatch { owner: String },
}

/// RAII wrapper around an active turn's result sender. Guarantees the
/// consumer's `Turn` stream always ends with a terminal event: on `Drop`
/// without a prior `mark_complete()` it `try_send`s a `TurnError`, so any
/// teardown path that drops the `ActiveTurn` without going through
/// `stream_turn_result` — notably the keepalive reap of a worker that
/// connected but never streamed a result — still unblocks the harness.
pub struct TurnResultGuard {
    tx: mpsc::Sender<TurnResultChunk>,
    completed: bool,
}

impl TurnResultGuard {
    pub fn new(tx: mpsc::Sender<TurnResultChunk>) -> Self {
        Self {
            tx,
            completed: false,
        }
    }

    /// Borrow the sender to forward chunks during normal streaming.
    pub fn sender(&self) -> &mpsc::Sender<TurnResultChunk> {
        &self.tx
    }

    /// Mark the turn as completed by `stream_turn_result`, so `Drop` does not
    /// emit a second, spurious terminal.
    pub fn mark_complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for TurnResultGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let _ = self.tx.try_send(TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Error(TurnError {
                code: tonic::Code::Unavailable as i32,
                message: "turn terminated without completion (worker reaped or vanished)"
                    .to_string(),
            })),
        });
    }
}

pub struct ActiveTurn {
    pub result_tx: TurnResultGuard,
    pub workspace: String,
    pub conversation_id: String,
    pub reply_channel: Option<String>,
    pub role: Option<TurnRole>,
    pub correlation_id: Option<String>,
    pub system_prompt: Option<String>,
}

struct ModelSlot {
    spec: ModelSpec,
    pending_tx: mpsc::Sender<PendingTurn>,
    pending_rx: Mutex<mpsc::Receiver<PendingTurn>>,
    active_turn: Mutex<Option<ActiveTurn>>,
    job_connected: Mutex<bool>,
    job_notify: Notify,
    last_activity: Mutex<Instant>,
    active_job_name: Mutex<Option<String>>,
}

impl ModelSlot {
    fn new(spec: ModelSpec) -> Self {
        let (pending_tx, pending_rx) = mpsc::channel(1);
        Self {
            spec,
            pending_tx,
            pending_rx: Mutex::new(pending_rx),
            active_turn: Mutex::new(None),
            job_connected: Mutex::new(false),
            job_notify: Notify::new(),
            last_activity: Mutex::new(Instant::now()),
            active_job_name: Mutex::new(None),
        }
    }
}

// =========================================================================
// Unified controller state
// =========================================================================

/// Per-`(workspace, tool_name)` spawn mutex map: held across the tool Job
/// get-probe-create sequence so two concurrent calls for the same worker
/// cannot both spawn, while distinct workspaces never contend.
type ToolDispatchLocks = HashMap<(String, String), Arc<Mutex<()>>>;

pub struct ControllerState {
    // -- Tool dispatch --
    tools: RwLock<HashMap<String, RegisteredTool>>,
    /// Monotonic counter bumped on every mutation of `tools`. The `WatchTools`
    /// handler holds a `watch::Receiver<u64>` and `.changed().await` to be
    /// woken when the registry changes.
    tools_revision: watch::Sender<u64>,
    toolsets: RwLock<HashMap<String, Toolset>>,
    /// Pending tool calls keyed by `(workspace, tool_name)`. `workspace` comes
    /// from the authenticated caller, so one workspace's worker can only dequeue
    /// its own calls, never another workspace's queued call for the same tool.
    pending_calls: RwLock<HashMap<(String, String), Vec<PendingCall>>>,
    call_notify: Notify,
    result_txs: RwLock<HashMap<String, ToolResultGuard>>,
    result_rxs: RwLock<HashMap<String, mpsc::Receiver<ToolResultFrame>>>,
    call_cancel_tokens: RwLock<HashMap<String, CancellationToken>>,
    call_id_to_tool: RwLock<HashMap<String, (String, String)>>,
    active_jobs: RwLock<HashMap<(String, String), ActiveJob>>,
    tool_dispatch_locks: RwLock<ToolDispatchLocks>,

    // -- Turn dispatch --
    models: RwLock<HashMap<String, Arc<ModelSlot>>>,
    providers: RwLock<HashMap<String, ProviderSpec>>,
    /// Per-turn cancellation tokens keyed by `(workspace, conversation_id)`.
    /// `workspace` comes from the authenticated caller, never the payload, so
    /// a cancel cannot fire another tenant's turn.
    turn_cancel_tokens: RwLock<HashMap<(String, String), CancellationToken>>,

    // -- Shared --
    kube_client: Option<kube::Client>,
    namespace: String,
    controller_addr: String,
    prompt_job_image: String,
    scheduling: SchedulingConfig,
}

impl ControllerState {
    pub fn new(
        kube_client: Option<kube::Client>,
        namespace: String,
        controller_addr: String,
        prompt_job_image: String,
        scheduling: SchedulingConfig,
    ) -> Arc<Self> {
        let (tools_revision, _) = watch::channel(0u64);
        Arc::new(Self {
            tools: RwLock::new(HashMap::new()),
            tools_revision,
            toolsets: RwLock::new(HashMap::new()),
            pending_calls: RwLock::new(HashMap::new()),
            call_notify: Notify::new(),
            result_txs: RwLock::new(HashMap::new()),
            result_rxs: RwLock::new(HashMap::new()),
            call_cancel_tokens: RwLock::new(HashMap::new()),
            call_id_to_tool: RwLock::new(HashMap::new()),
            active_jobs: RwLock::new(HashMap::new()),
            tool_dispatch_locks: RwLock::new(HashMap::new()),
            models: RwLock::new(HashMap::new()),
            providers: RwLock::new(HashMap::new()),
            turn_cancel_tokens: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            controller_addr,
            prompt_job_image,
            scheduling,
        })
    }

    pub fn kube_client(&self) -> Option<&kube::Client> {
        self.kube_client.as_ref()
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn controller_addr(&self) -> &str {
        &self.controller_addr
    }

    pub fn prompt_job_image(&self) -> &str {
        &self.prompt_job_image
    }

    pub fn scheduling(&self) -> &SchedulingConfig {
        &self.scheduling
    }

    // ---- Tool registry ----

    pub fn subscribe_tools_revision(&self) -> watch::Receiver<u64> {
        self.tools_revision.subscribe()
    }

    pub async fn get_tool(&self, name: &str) -> Option<RegisteredTool> {
        self.tools.read().await.get(name).cloned()
    }

    pub async fn list_tools(&self) -> Vec<(String, RegisteredTool)> {
        self.tools
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub async fn list_tools_for_workspace(
        &self,
        workspace: &str,
        bindings: &WorkspaceBindings,
    ) -> Vec<(String, RegisteredTool)> {
        let toolsets = bindings.toolsets_for(workspace);
        if toolsets.is_empty() {
            return vec![];
        }
        self.tools
            .read()
            .await
            .iter()
            .filter(|(_, tool)| toolsets.iter().any(|c| c == &tool.toolset_name))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub async fn set_tools_for_toolset(&self, toolset_name: &str, tools: Vec<RegisteredTool>) {
        let mut registry = self.tools.write().await;
        registry.retain(|_, t| t.toolset_name != toolset_name);
        for tool in tools {
            if registry.contains_key(&tool.name) {
                warn!(
                    tool = %tool.name,
                    toolset = %toolset_name,
                    "duplicate tool name, first toolset wins"
                );
                continue;
            }
            registry.insert(tool.name.clone(), tool);
        }
        drop(registry);
        self.tools_revision.send_modify(|r| *r += 1);
    }

    pub async fn remove_tools_for_toolset(&self, toolset_name: &str) {
        self.tools
            .write()
            .await
            .retain(|_, t| t.toolset_name != toolset_name);
        self.tools_revision.send_modify(|r| *r += 1);
    }

    pub async fn clear_tools(&self) {
        self.tools.write().await.clear();
        self.tools_revision.send_modify(|r| *r += 1);
    }

    pub async fn tool_count(&self) -> usize {
        self.tools.read().await.len()
    }

    /// Whether any registered tool is bound to `toolset_name`. Used by the
    /// Turn handler to reject a name collision between a prompt toolset and a
    /// tool-bearing toolset.
    pub async fn toolset_has_tools(&self, toolset_name: &str) -> bool {
        self.tools
            .read()
            .await
            .values()
            .any(|t| t.toolset_name == toolset_name)
    }

    // ---- Toolset registry ----

    pub async fn get_toolset(&self, name: &str) -> Option<Toolset> {
        self.toolsets.read().await.get(name).cloned()
    }

    pub async fn set_toolset(&self, name: String, toolset: Toolset) {
        self.toolsets.write().await.insert(name, toolset);
    }

    pub async fn remove_toolset(&self, name: &str) {
        self.toolsets.write().await.remove(name);
    }

    pub async fn clear_toolsets(&self) {
        self.toolsets.write().await.clear();
    }

    pub async fn toolset_count(&self) -> usize {
        self.toolsets.read().await.len()
    }

    /// The set of registered Toolset CR names — the input to
    /// [`crate::resolve_prompt_toolset`].
    pub async fn registered_toolset_names(&self) -> std::collections::BTreeSet<String> {
        self.toolsets.read().await.keys().cloned().collect()
    }

    // ---- Call queue ----

    pub async fn enqueue_call(&self, call: PendingCall) {
        self.pending_calls
            .write()
            .await
            .entry((call.workspace.clone(), call.tool_name.clone()))
            .or_default()
            .push(call);
        self.call_notify.notify_waiters();
    }

    pub async fn dequeue_call(&self, workspace: &str, tool_name: &str) -> Option<PendingCall> {
        let mut pending = self.pending_calls.write().await;
        let calls = pending.get_mut(&(workspace.to_string(), tool_name.to_string()))?;
        if calls.is_empty() {
            None
        } else {
            Some(calls.remove(0))
        }
    }

    pub async fn wait_for_call(&self) {
        self.call_notify.notified().await;
    }

    // ---- Tool result channels ----

    pub async fn set_result_tx(
        &self,
        call_id: String,
        workspace: String,
        tool_name: String,
        tx: mpsc::Sender<ToolResultFrame>,
    ) {
        self.result_txs
            .write()
            .await
            .insert(call_id.clone(), ToolResultGuard::new(tx));
        self.call_id_to_tool
            .write()
            .await
            .insert(call_id, (workspace, tool_name));
    }

    /// Drains both the result channel and the `call_id -> (workspace,
    /// tool_name)` shadow entry, returning the worker key alongside the sender.
    pub async fn take_result_tx(
        &self,
        call_id: &str,
    ) -> Option<(ToolResultGuard, (String, String))> {
        let tx = self.result_txs.write().await.remove(call_id)?;
        let worker = self
            .call_id_to_tool
            .write()
            .await
            .remove(call_id)
            .unwrap_or_default();
        Some((tx, worker))
    }

    /// Drain every pending result sender whose call is bound to `(workspace,
    /// tool_name)`. Used by the reap path: dropping the returned guards fires
    /// each one's synthetic error terminal, unblocking any harness streaming a
    /// toolset that was torn down. Keying on the workspace too stops one
    /// workspace's worker expiry from firing terminals on another workspace's
    /// parked calls for the same tool.
    pub async fn take_result_txs_for_worker(
        &self,
        workspace: &str,
        tool_name: &str,
    ) -> Vec<ToolResultGuard> {
        let target = (workspace.to_string(), tool_name.to_string());
        let call_ids: Vec<String> = {
            let shadow = self.call_id_to_tool.read().await;
            shadow
                .iter()
                .filter(|(_, key)| **key == target)
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
            let mut shadow = self.call_id_to_tool.write().await;
            for call_id in &call_ids {
                shadow.remove(call_id);
            }
        }
        guards
    }

    pub async fn set_result_rx(&self, call_id: String, rx: mpsc::Receiver<ToolResultFrame>) {
        self.result_rxs.write().await.insert(call_id, rx);
    }

    pub async fn take_result_rx(&self, call_id: &str) -> Option<mpsc::Receiver<ToolResultFrame>> {
        self.result_rxs.write().await.remove(call_id)
    }

    // ---- Per-call cancellation ----

    pub async fn register_call_cancel(&self, call_id: String) {
        self.call_cancel_tokens
            .write()
            .await
            .insert(call_id, CancellationToken::new());
    }

    pub async fn call_cancel_token(&self, call_id: &str) -> Option<CancellationToken> {
        self.call_cancel_tokens.read().await.get(call_id).cloned()
    }

    pub async fn fire_call_cancel(&self, call_id: &str) -> bool {
        match self.call_cancel_tokens.write().await.remove(call_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Drop bookkeeping for a completed call: its parked receiver and its
    /// cancellation token. Idempotent; called when `await_tool_result` returns.
    pub async fn finish_call(&self, call_id: &str) {
        self.result_rxs.write().await.remove(call_id);
        self.call_cancel_tokens.write().await.remove(call_id);
    }

    // ---- Active tool Jobs (keepalive) ----

    pub async fn list_active_jobs(&self) -> Vec<((String, String), String, u64, Instant)> {
        self.active_jobs
            .read()
            .await
            .iter()
            .map(|(key, job)| {
                (
                    key.clone(),
                    job.job_name.clone(),
                    job.keepalive_seconds,
                    job.last_activity,
                )
            })
            .collect()
    }

    pub async fn get_active_job(&self, workspace: &str, tool_name: &str) -> Option<ActiveJob> {
        self.active_jobs
            .read()
            .await
            .get(&(workspace.to_string(), tool_name.to_string()))
            .cloned()
    }

    /// Insert an active tool Job, keyed by `(workspace, tool_name)` drawn from
    /// the job itself so the key and stored identity cannot drift apart.
    pub async fn set_active_job(&self, job: ActiveJob) {
        let key = (job.workspace.clone(), job.tool_name.clone());
        self.active_jobs.write().await.insert(key, job);
    }

    pub async fn remove_active_job(&self, workspace: &str, tool_name: &str) {
        self.active_jobs
            .write()
            .await
            .remove(&(workspace.to_string(), tool_name.to_string()));
    }

    pub async fn bump_last_activity(&self, workspace: &str, tool_name: &str) {
        if let Some(j) = self
            .active_jobs
            .write()
            .await
            .get_mut(&(workspace.to_string(), tool_name.to_string()))
        {
            j.last_activity = Instant::now();
        }
    }

    pub async fn active_job_count(&self) -> usize {
        self.active_jobs.read().await.len()
    }

    pub async fn tool_dispatch_lock(&self, workspace: &str, tool_name: &str) -> Arc<Mutex<()>> {
        let key = (workspace.to_string(), tool_name.to_string());
        if let Some(m) = self.tool_dispatch_locks.read().await.get(&key) {
            return m.clone();
        }
        let mut w = self.tool_dispatch_locks.write().await;
        w.entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    // ---- Model registry ----

    pub async fn set_model_spec(&self, name: String, spec: ModelSpec) {
        let mut models = self.models.write().await;
        models.insert(name, Arc::new(ModelSlot::new(spec)));
    }

    pub async fn remove_model(&self, name: &str) {
        self.models.write().await.remove(name);
    }

    pub async fn clear_models(&self) {
        self.models.write().await.clear();
    }

    pub async fn get_model_spec(&self, name: &str) -> Option<ModelSpec> {
        self.models.read().await.get(name).map(|s| s.spec.clone())
    }

    /// Reserved-name fallback: prefer a model literally named `default`,
    /// otherwise the alphabetic-first registered model.
    pub async fn default_or_alphabetic_first(&self) -> Option<String> {
        let models = self.models.read().await;
        if models.contains_key("default") {
            return Some("default".to_string());
        }
        let mut keys: Vec<&String> = models.keys().collect();
        keys.sort();
        keys.first().map(|s| (*s).clone())
    }

    // ---- Provider registry ----

    pub async fn set_provider_spec(&self, name: String, spec: ProviderSpec) {
        self.providers.write().await.insert(name, spec);
    }

    pub async fn get_provider(&self, name: &str) -> Option<ProviderSpec> {
        self.providers.read().await.get(name).cloned()
    }

    pub async fn remove_provider(&self, name: &str) {
        self.providers.write().await.remove(name);
    }

    pub async fn clear_providers(&self) {
        self.providers.write().await.clear();
    }

    // ---- Per-turn cancellation, keyed by (workspace, conversation_id) ----

    fn cancel_key(workspace: &str, conversation_id: &str) -> (String, String) {
        (workspace.to_string(), conversation_id.to_string())
    }

    pub async fn register_cancel(&self, workspace: &str, conversation_id: &str) {
        self.turn_cancel_tokens.write().await.insert(
            Self::cancel_key(workspace, conversation_id),
            CancellationToken::new(),
        );
    }

    pub async fn cancel_token(
        &self,
        workspace: &str,
        conversation_id: &str,
    ) -> Option<CancellationToken> {
        self.turn_cancel_tokens
            .read()
            .await
            .get(&Self::cancel_key(workspace, conversation_id))
            .cloned()
    }

    pub async fn fire_cancel(&self, workspace: &str, conversation_id: &str) -> bool {
        match self
            .turn_cancel_tokens
            .write()
            .await
            .remove(&Self::cancel_key(workspace, conversation_id))
        {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    pub async fn finish_turn(&self, workspace: &str, conversation_id: &str) {
        self.turn_cancel_tokens
            .write()
            .await
            .remove(&Self::cancel_key(workspace, conversation_id));
    }

    // ---- Model slots (turn dispatch lifecycle) ----

    async fn get_slot(&self, model: &str) -> Option<Arc<ModelSlot>> {
        self.models.read().await.get(model).cloned()
    }

    pub async fn check_job_needed(&self, model: &str) -> JobAction {
        let slot = match self.get_slot(model).await {
            Some(s) => s,
            None => return JobAction::NoModelSpec,
        };
        if *slot.job_connected.lock().await {
            return JobAction::AlreadyConnected;
        }
        let provider_name = slot.spec.provider_ref.name.clone();
        let provider = match self.get_provider(&provider_name).await {
            Some(p) => p,
            None => return JobAction::NoProviderSpec(provider_name),
        };
        if self.kube_client.is_none() {
            return JobAction::NoKubeClient;
        }
        JobAction::Create(Box::new(JobCreateSpec {
            model: slot.spec.clone(),
            provider,
        }))
    }

    pub async fn enqueue_turn(&self, model: &str, pending: PendingTurn) -> Result<(), String> {
        let slot = self
            .get_slot(model)
            .await
            .ok_or_else(|| format!("no model slot for {model}"))?;
        slot.pending_tx
            .send(pending)
            .await
            .map_err(|_| "turn queue closed".to_string())
    }

    pub async fn wait_for_turn(&self, model: &str) -> Option<PendingTurn> {
        let slot = self.get_slot(model).await?;
        let mut rx = slot.pending_rx.lock().await;
        rx.recv().await
    }

    /// Drain every never-claimed pending turn buffered on `model`'s slot.
    pub async fn drain_pending_turns(&self, model: &str) -> Vec<PendingTurn> {
        let Some(slot) = self.get_slot(model).await else {
            return Vec::new();
        };
        // A held `pending_rx` guard implies an empty channel (capacity-1
        // rendezvous), so on contention there is nothing to drain — skip.
        let Ok(mut rx) = slot.pending_rx.try_lock() else {
            return Vec::new();
        };
        let mut drained = Vec::new();
        while let Ok(pending) = rx.try_recv() {
            drained.push(pending);
        }
        drained
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn set_active_turn(
        &self,
        model: &str,
        workspace: String,
        conversation_id: String,
        reply_channel: Option<String>,
        role: Option<TurnRole>,
        correlation_id: Option<String>,
        system_prompt: Option<String>,
        tx: mpsc::Sender<TurnResultChunk>,
    ) {
        if let Some(slot) = self.get_slot(model).await {
            *slot.active_turn.lock().await = Some(ActiveTurn {
                result_tx: TurnResultGuard::new(tx),
                workspace,
                conversation_id,
                reply_channel,
                role,
                correlation_id,
                system_prompt,
            });
        }
    }

    /// Take the active turn for `model` if it is owned by `caller_workspace`.
    /// Lock-scoped peek-then-take eliminates TOCTOU. On `OwnerMismatch` the
    /// slot stays intact so the legitimate caller can still claim it.
    pub async fn take_active_turn_if_owned(
        &self,
        model: &str,
        caller_workspace: &str,
    ) -> Result<ActiveTurn, TakeTurnError> {
        let Some(slot) = self.get_slot(model).await else {
            return Err(TakeTurnError::NoActiveTurn);
        };
        let mut guard = slot.active_turn.lock().await;
        match guard.as_ref() {
            None => Err(TakeTurnError::NoActiveTurn),
            Some(active) if active.workspace != caller_workspace => {
                Err(TakeTurnError::OwnerMismatch {
                    owner: active.workspace.clone(),
                })
            }
            Some(_) => Ok(guard.take().expect("guard had Some")),
        }
    }

    /// Unconditionally take the active turn for `model`, regardless of owner.
    /// Used by teardown paths (the keepalive reap, and the Job watch): dropping
    /// the returned `ActiveTurn` fires its `TurnResultGuard`.
    pub async fn take_active_turn(&self, model: &str) -> Option<ActiveTurn> {
        let slot = self.get_slot(model).await?;
        let mut guard = slot.active_turn.lock().await;
        guard.take()
    }

    pub async fn set_job_connected(&self, model: &str, connected: bool) {
        if let Some(slot) = self.get_slot(model).await {
            *slot.job_connected.lock().await = connected;
            if connected {
                slot.job_notify.notify_waiters();
            }
        }
    }

    pub async fn wait_for_job_connect(&self, model: &str, timeout: Duration) -> bool {
        let slot = match self.get_slot(model).await {
            Some(s) => s,
            None => return false,
        };
        if *slot.job_connected.lock().await {
            return true;
        }
        tokio::time::timeout(timeout, slot.job_notify.notified())
            .await
            .is_ok()
    }

    pub async fn bump_model_activity(&self, model: &str) {
        if let Some(slot) = self.get_slot(model).await {
            *slot.last_activity.lock().await = Instant::now();
        }
    }

    pub async fn set_active_llm_job(&self, model: &str, job_name: Option<String>) {
        if let Some(slot) = self.get_slot(model).await {
            *slot.active_job_name.lock().await = job_name;
        }
    }

    pub async fn list_idle_models(&self, idle: Duration, now: Instant) -> Vec<(String, String)> {
        let models = self.models.read().await;
        let mut out = Vec::new();
        for (name, slot) in models.iter() {
            let job_name = match slot.active_job_name.lock().await.clone() {
                Some(n) => n,
                None => continue,
            };
            let last = *slot.last_activity.lock().await;
            if now.saturating_duration_since(last) >= idle {
                out.push((name.clone(), job_name));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{ProviderRef, ProviderSecret, ToolsetSpec};

    fn test_toolset(name: &str) -> Toolset {
        Toolset::new(
            name,
            ToolsetSpec {
                image: None,
                credentials: vec![],
                egress: vec![],
                keepalive: false,
            },
        )
    }

    fn test_registered_tool(name: &str, toolset: &str) -> RegisteredTool {
        RegisteredTool {
            name: name.to_string(),
            toolset_name: toolset.to_string(),
            description: format!("Execute a {name} command."),
            args: vec![],
        }
    }

    fn test_state() -> Arc<ControllerState> {
        ControllerState::new(
            None,
            String::new(),
            String::new(),
            "ghcr.io/test/prompt-job:latest".into(),
            SchedulingConfig::default(),
        )
    }

    fn test_model_spec() -> ModelSpec {
        ModelSpec {
            provider_ref: ProviderRef {
                name: "anthropic".into(),
            },
            model: "claude-sonnet-4-20250514".into(),
            params: None,
        }
    }

    fn test_provider_spec() -> ProviderSpec {
        ProviderSpec {
            format: "anthropic".into(),
            base_url: Some("https://api.anthropic.com/v1".into()),
            secret: ProviderSecret {
                name: "anthropic-key".into(),
                key: None,
            },
        }
    }

    // ---- Tool registry ----

    #[tokio::test]
    async fn tool_count_reflects_insertions() {
        let state = test_state();
        assert_eq!(state.tool_count().await, 0);
        state
            .set_tools_for_toolset(
                "c1",
                vec![
                    test_registered_tool("git", "c1"),
                    test_registered_tool("gh", "c1"),
                ],
            )
            .await;
        assert_eq!(state.tool_count().await, 2);
    }

    #[tokio::test]
    async fn clear_tools_empties_registry() {
        let state = test_state();
        state
            .set_tools_for_toolset("c1", vec![test_registered_tool("git", "c1")])
            .await;
        state.clear_tools().await;
        assert_eq!(state.tool_count().await, 0);
    }

    #[tokio::test]
    async fn set_tools_replaces_toolset_tools() {
        let state = test_state();
        state
            .set_tools_for_toolset("c1", vec![test_registered_tool("git", "c1")])
            .await;
        state
            .set_tools_for_toolset("c1", vec![test_registered_tool("gh", "c1")])
            .await;
        assert_eq!(state.tool_count().await, 1);
        assert!(state.get_tool("gh").await.is_some());
        assert!(state.get_tool("git").await.is_none());
    }

    #[tokio::test]
    async fn remove_tools_for_toolset_only_affects_that_toolset() {
        let state = test_state();
        state
            .set_tools_for_toolset("c1", vec![test_registered_tool("git", "c1")])
            .await;
        state
            .set_tools_for_toolset("c2", vec![test_registered_tool("gh", "c2")])
            .await;
        state.remove_tools_for_toolset("c1").await;
        assert_eq!(state.tool_count().await, 1);
        assert!(state.get_tool("gh").await.is_some());
    }

    #[tokio::test]
    async fn duplicate_tool_name_first_toolset_wins() {
        let state = test_state();
        state
            .set_tools_for_toolset("c1", vec![test_registered_tool("git", "c1")])
            .await;
        state
            .set_tools_for_toolset("c2", vec![test_registered_tool("git", "c2")])
            .await;
        assert_eq!(state.tool_count().await, 1);
        let tool = state.get_tool("git").await.unwrap();
        assert_eq!(tool.toolset_name, "c1");
    }

    #[tokio::test]
    async fn toolset_has_tools_reflects_registration() {
        let state = test_state();
        assert!(!state.toolset_has_tools("git").await);
        state
            .set_tools_for_toolset("git", vec![test_registered_tool("git-push", "git")])
            .await;
        assert!(state.toolset_has_tools("git").await);
        assert!(!state.toolset_has_tools("other").await);
    }

    #[tokio::test]
    async fn registered_toolset_names_reflects_insertions() {
        let state = test_state();
        state.set_toolset("a".into(), test_toolset("a")).await;
        state.set_toolset("b".into(), test_toolset("b")).await;
        let names = state.registered_toolset_names().await;
        assert!(names.contains("a"));
        assert!(names.contains("b"));
        assert_eq!(names.len(), 2);
    }

    #[tokio::test]
    async fn toolset_count_reflects_insertions() {
        let state = test_state();
        assert_eq!(state.toolset_count().await, 0);
        state.set_toolset("a".into(), test_toolset("a")).await;
        state.set_toolset("b".into(), test_toolset("b")).await;
        assert_eq!(state.toolset_count().await, 2);
    }

    #[tokio::test]
    async fn clear_toolsets_empties_registry() {
        let state = test_state();
        state.set_toolset("a".into(), test_toolset("a")).await;
        state.clear_toolsets().await;
        assert_eq!(state.toolset_count().await, 0);
    }

    #[tokio::test]
    async fn wait_for_call_blocks_until_notify() {
        let state = test_state();
        let state2 = state.clone();

        let wait_handle = tokio::spawn(async move {
            state2.wait_for_call().await;
        });

        tokio::task::yield_now().await;
        assert!(!wait_handle.is_finished(), "should be blocking");

        state
            .enqueue_call(PendingCall {
                call_id: "c".into(),
                tool_name: "t".into(),
                workspace: "w".into(),
                args: HashMap::new(),
                working_dir: "/w".into(),
            })
            .await;

        tokio::time::timeout(std::time::Duration::from_secs(2), wait_handle)
            .await
            .expect("wait_for_call should unblock")
            .unwrap();
    }

    #[test]
    fn bindings_has_toolset_true_for_bound() {
        let mut map = HashMap::new();
        map.insert(
            "ws1".to_string(),
            vec!["git".to_string(), "ssh".to_string()],
        );
        let bindings = WorkspaceBindings::from_map(map);
        assert!(bindings.has_toolset("ws1", "git"));
        assert!(bindings.has_toolset("ws1", "ssh"));
    }

    #[test]
    fn bindings_has_toolset_false_for_unbound() {
        let mut map = HashMap::new();
        map.insert("ws1".to_string(), vec!["git".to_string()]);
        let bindings = WorkspaceBindings::from_map(map);
        assert!(!bindings.has_toolset("ws1", "ssh"));
    }

    #[test]
    fn bindings_has_toolset_false_for_unknown_workspace() {
        let bindings = WorkspaceBindings::empty();
        assert!(!bindings.has_toolset("nonexistent", "git"));
    }

    #[test]
    fn bindings_toolsets_for_unknown_returns_empty() {
        let bindings = WorkspaceBindings::empty();
        assert!(bindings.toolsets_for("nonexistent").is_empty());
    }

    #[tokio::test]
    async fn list_tools_for_workspace_filters_by_binding() {
        let state = test_state();
        state
            .set_tools_for_toolset("git", vec![test_registered_tool("git-push", "git")])
            .await;
        state
            .set_tools_for_toolset("ssh", vec![test_registered_tool("ssh-exec", "ssh")])
            .await;

        let mut map = HashMap::new();
        map.insert("ws1".to_string(), vec!["git".to_string()]);
        let bindings = WorkspaceBindings::from_map(map);

        let tools = state.list_tools_for_workspace("ws1", &bindings).await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0, "git-push");
    }

    #[tokio::test]
    async fn list_tools_for_workspace_unknown_returns_empty() {
        let state = test_state();
        state
            .set_tools_for_toolset("git", vec![test_registered_tool("git-push", "git")])
            .await;
        let bindings = WorkspaceBindings::empty();
        let tools = state.list_tools_for_workspace("unknown", &bindings).await;
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn set_tools_for_toolset_bumps_revision() {
        let state = test_state();
        let mut rx = state.subscribe_tools_revision();
        rx.mark_unchanged();
        state
            .set_tools_for_toolset("c1", vec![test_registered_tool("git", "c1")])
            .await;
        tokio::time::timeout(std::time::Duration::from_millis(100), rx.changed())
            .await
            .expect("revision must change after set_tools_for_toolset")
            .expect("sender must still be alive");
    }

    #[tokio::test]
    async fn remove_tools_for_toolset_bumps_revision() {
        let state = test_state();
        state
            .set_tools_for_toolset("c1", vec![test_registered_tool("git", "c1")])
            .await;
        let mut rx = state.subscribe_tools_revision();
        rx.mark_unchanged();
        state.remove_tools_for_toolset("c1").await;
        tokio::time::timeout(std::time::Duration::from_millis(100), rx.changed())
            .await
            .expect("revision must change after remove_tools_for_toolset")
            .expect("sender must still be alive");
    }

    #[tokio::test]
    async fn clear_tools_bumps_revision() {
        let state = test_state();
        let mut rx = state.subscribe_tools_revision();
        rx.mark_unchanged();
        state.clear_tools().await;
        tokio::time::timeout(std::time::Duration::from_millis(100), rx.changed())
            .await
            .expect("revision must change after clear_tools")
            .expect("sender must still be alive");
    }

    // ---- Active tool jobs ----

    #[tokio::test]
    async fn get_active_job_returns_set_value() {
        let state = test_state();
        state
            .set_active_job(ActiveJob {
                job_name: "airlock-search-abc".into(),
                tool_name: "Search".into(),
                workspace: "ws".into(),
                last_activity: Instant::now(),
                keepalive_seconds: 600,
            })
            .await;

        let got = state.get_active_job("ws", "Search").await.expect("present");
        assert_eq!(got.job_name, "airlock-search-abc");
        assert_eq!(got.tool_name, "Search");
        assert_eq!(got.keepalive_seconds, 600);
        assert!(state.get_active_job("ws", "absent").await.is_none());
    }

    #[tokio::test]
    async fn bump_last_activity_updates_timestamp() {
        let state = test_state();
        let started = Instant::now() - std::time::Duration::from_secs(10);
        state
            .set_active_job(ActiveJob {
                job_name: "airlock-shell-abc".into(),
                tool_name: "Shell".into(),
                workspace: "ws".into(),
                last_activity: started,
                keepalive_seconds: 600,
            })
            .await;

        state.bump_last_activity("ws", "Shell").await;

        let got = state.get_active_job("ws", "Shell").await.unwrap();
        assert!(
            got.last_activity > started,
            "last_activity must advance on bump"
        );

        state.bump_last_activity("ws", "Nope").await;
        assert!(state.get_active_job("ws", "Nope").await.is_none());
    }

    #[tokio::test]
    async fn tool_dispatch_lock_returns_same_mutex_per_tool() {
        let state = test_state();
        let a = state.tool_dispatch_lock("ws", "Search").await;
        let b = state.tool_dispatch_lock("ws", "Search").await;
        let c = state.tool_dispatch_lock("ws", "Read").await;

        assert!(Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&a, &c));

        let _g = a.lock().await;
        assert!(b.try_lock().is_err());
    }

    #[tokio::test]
    async fn tool_dispatch_lock_is_per_workspace() {
        let state = test_state();
        let a = state.tool_dispatch_lock("ws-a", "Search").await;
        let b = state.tool_dispatch_lock("ws-b", "Search").await;
        assert!(
            !Arc::ptr_eq(&a, &b),
            "two workspaces racing the same tool must not share a spawn lock"
        );
    }

    #[tokio::test]
    async fn set_take_result_tx_round_trips_worker_key() {
        let state = test_state();
        let (tx, _rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        state
            .set_result_tx("call-1".into(), "ws".into(), "Search".into(), tx)
            .await;

        let (_tx_back, (workspace, tool_name)) =
            state.take_result_tx("call-1").await.expect("present");
        assert_eq!(workspace, "ws");
        assert_eq!(tool_name, "Search");
        assert!(state.take_result_tx("call-1").await.is_none());
    }

    #[tokio::test]
    async fn dequeue_call_isolates_by_workspace() {
        let state = test_state();
        state
            .enqueue_call(PendingCall {
                call_id: "b-call".into(),
                tool_name: "shell".into(),
                workspace: "workspace-b".into(),
                args: HashMap::new(),
                working_dir: "/w".into(),
            })
            .await;

        assert!(
            state.dequeue_call("workspace-a", "shell").await.is_none(),
            "workspace-a must not dequeue workspace-b's queued call for the same tool"
        );
        let call = state
            .dequeue_call("workspace-b", "shell")
            .await
            .expect("the owning workspace dequeues its own call");
        assert_eq!(call.call_id, "b-call");
    }

    #[tokio::test]
    async fn active_jobs_keyed_by_workspace() {
        let state = test_state();
        state
            .set_active_job(ActiveJob {
                job_name: "airlock-shell-a".into(),
                tool_name: "shell".into(),
                workspace: "workspace-a".into(),
                last_activity: Instant::now(),
                keepalive_seconds: 600,
            })
            .await;

        assert!(
            state.get_active_job("workspace-b", "shell").await.is_none(),
            "workspace-b must not resolve workspace-a's active job for the same tool"
        );
        assert!(state.get_active_job("workspace-a", "shell").await.is_some());
    }

    #[tokio::test]
    async fn take_result_txs_for_worker_isolates_by_workspace() {
        let state = test_state();
        let (tx_a, _rx_a) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        let (tx_b, mut rx_b) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        state
            .set_result_tx("call-a".into(), "workspace-a".into(), "shell".into(), tx_a)
            .await;
        state
            .set_result_tx("call-b".into(), "workspace-b".into(), "shell".into(), tx_b)
            .await;

        // Reaping workspace-a's worker must leave workspace-b's parked call intact.
        drop(
            state
                .take_result_txs_for_worker("workspace-a", "shell")
                .await,
        );

        assert!(
            rx_b.try_recv().is_err(),
            "workspace-b's parked call must not receive a synthetic terminal from workspace-a's reap"
        );
        assert!(
            state.take_result_tx("call-b").await.is_some(),
            "workspace-b's result sender must still be present after workspace-a's reap"
        );
    }

    // ---- Model / provider registry ----

    #[tokio::test]
    async fn enqueue_and_wait_delivers() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;

        let (result_tx, _result_rx) = mpsc::channel(1);
        let pending = PendingTurn {
            assignment: TurnAssignment {
                system: Some("test".into()),
                tools: vec![],
                messages: vec![],
                params_json: None,
                conversation_id: "test-conv".into(),
            },
            result_tx,
            workspace: "default".into(),
            conversation_id: "test-conv".into(),
            reply_channel: None,
            role: None,
            correlation_id: None,
            system_prompt: None,
        };

        let state_clone = state.clone();
        let handle = tokio::spawn(async move { state_clone.wait_for_turn("default").await });

        state.enqueue_turn("default", pending).await.unwrap();
        let received = handle.await.unwrap().unwrap();
        assert_eq!(received.assignment.system, Some("test".into()));
    }

    fn test_pending() -> (PendingTurn, mpsc::Receiver<TurnResultChunk>) {
        let (result_tx, result_rx) = mpsc::channel(1);
        let pending = PendingTurn {
            assignment: TurnAssignment {
                system: Some("test".into()),
                tools: vec![],
                messages: vec![],
                params_json: None,
                conversation_id: "test-conv".into(),
            },
            result_tx,
            workspace: "default".into(),
            conversation_id: "test-conv".into(),
            reply_channel: None,
            role: None,
            correlation_id: None,
            system_prompt: None,
        };
        (pending, result_rx)
    }

    #[tokio::test]
    async fn drain_pending_turns_does_not_block_a_parked_wait_for_turn() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;

        let state_clone = state.clone();
        let parked = tokio::spawn(async move { state_clone.wait_for_turn("default").await });
        tokio::time::sleep(Duration::from_millis(20)).await;

        let drained = tokio::time::timeout(
            Duration::from_millis(50),
            state.drain_pending_turns("default"),
        )
        .await
        .expect("drain must not block behind the parked wait_for_turn guard");
        assert!(
            drained.is_empty(),
            "a held guard implies an empty channel; drain yields nothing"
        );

        parked.abort();
    }

    #[tokio::test]
    async fn drain_pending_turns_drains_buffered_turn_when_uncontended() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;

        let (pending, _result_rx) = test_pending();
        state.enqueue_turn("default", pending).await.unwrap();

        let drained = state.drain_pending_turns("default").await;
        assert_eq!(drained.len(), 1, "the buffered turn must be drained");
        assert_eq!(drained[0].conversation_id, "test-conv");
    }

    #[tokio::test]
    async fn take_active_turn_if_owned_returns_no_active_turn_when_empty() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        let result = state.take_active_turn_if_owned("default", "ws1").await;
        assert!(matches!(result, Err(TakeTurnError::NoActiveTurn)));
    }

    #[tokio::test]
    async fn set_then_take_active_turn_if_owned() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        let (tx, _rx) = mpsc::channel::<TurnResultChunk>(1);

        state
            .set_active_turn(
                "default",
                "ws1".into(),
                "test-conv".into(),
                None,
                None,
                None,
                None,
                tx,
            )
            .await;
        let turn = state
            .take_active_turn_if_owned("default", "ws1")
            .await
            .expect("matching workspace returns Ok");
        assert_eq!(turn.workspace, "ws1");
        assert!(
            matches!(
                state.take_active_turn_if_owned("default", "ws1").await,
                Err(TakeTurnError::NoActiveTurn),
            ),
            "second take should return NoActiveTurn",
        );
    }

    #[tokio::test]
    async fn take_active_turn_if_owned_returns_mismatch_without_taking() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        let (tx, _rx) = mpsc::channel::<TurnResultChunk>(1);

        state
            .set_active_turn(
                "default",
                "ws-a".into(),
                "test-conv".into(),
                None,
                None,
                None,
                None,
                tx,
            )
            .await;

        let mismatch = state.take_active_turn_if_owned("default", "ws-b").await;
        match mismatch {
            Err(TakeTurnError::OwnerMismatch { ref owner }) => assert_eq!(owner, "ws-a"),
            Err(TakeTurnError::NoActiveTurn) => panic!("expected OwnerMismatch, got NoActiveTurn"),
            Ok(_) => panic!("expected OwnerMismatch, got Ok"),
        }

        let legitimate = state
            .take_active_turn_if_owned("default", "ws-a")
            .await
            .expect("legitimate owner can still claim the turn");
        assert_eq!(legitimate.workspace, "ws-a");
    }

    fn chunk_error_code(chunk: &TurnResultChunk) -> Option<i32> {
        match &chunk.chunk {
            Some(turn_result_chunk::Chunk::Error(e)) => Some(e.code),
            _ => None,
        }
    }

    #[tokio::test]
    async fn turn_result_guard_drop_emits_terminal_error() {
        let (tx, mut rx) = mpsc::channel::<TurnResultChunk>(4);
        let guard = TurnResultGuard::new(tx);
        drop(guard);
        let chunk = rx.recv().await.expect("guard Drop must emit a terminal");
        assert_eq!(
            chunk_error_code(&chunk),
            Some(tonic::Code::Unavailable as i32)
        );
    }

    #[tokio::test]
    async fn turn_result_guard_marked_complete_drops_silently() {
        let (tx, mut rx) = mpsc::channel::<TurnResultChunk>(4);
        let mut guard = TurnResultGuard::new(tx);
        guard.mark_complete();
        drop(guard);
        assert!(
            rx.recv().await.is_none(),
            "a completed guard must not emit a terminal on drop"
        );
    }

    #[tokio::test]
    async fn take_active_turn_returns_then_clears() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        let (tx, _rx) = mpsc::channel::<TurnResultChunk>(4);
        state
            .set_active_turn(
                "default",
                "ws1".into(),
                "default.c".into(),
                None,
                None,
                None,
                None,
                tx,
            )
            .await;
        assert!(state.take_active_turn("default").await.is_some());
        assert!(
            state.take_active_turn("default").await.is_none(),
            "second take must be None"
        );
    }

    #[tokio::test]
    async fn dropping_taken_turn_unblocks_parked_receiver() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        let (tx, mut rx) = mpsc::channel::<TurnResultChunk>(4);
        state
            .set_active_turn(
                "default",
                "ws1".into(),
                "default.c".into(),
                None,
                None,
                None,
                None,
                tx,
            )
            .await;
        let taken = state
            .take_active_turn("default")
            .await
            .expect("turn present");
        drop(taken);
        let chunk = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("must not hang")
            .expect("must receive a terminal");
        assert_eq!(
            chunk_error_code(&chunk),
            Some(tonic::Code::Unavailable as i32)
        );
    }

    #[tokio::test]
    async fn check_job_needed_no_model_spec() {
        let state = test_state();
        assert!(matches!(
            state.check_job_needed("nonexistent").await,
            JobAction::NoModelSpec
        ));
    }

    #[tokio::test]
    async fn check_job_needed_no_kube_client() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        state
            .set_provider_spec("anthropic".into(), test_provider_spec())
            .await;
        assert!(matches!(
            state.check_job_needed("default").await,
            JobAction::NoKubeClient
        ));
    }

    #[tokio::test]
    async fn check_job_needed_already_connected() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        state.set_job_connected("default", true).await;
        assert!(matches!(
            state.check_job_needed("default").await,
            JobAction::AlreadyConnected
        ));
    }

    #[tokio::test]
    async fn check_job_needed_returns_no_provider_spec_when_referenced_provider_missing() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        match state.check_job_needed("default").await {
            JobAction::NoProviderSpec(name) => assert_eq!(name, "anthropic"),
            other => panic!(
                "expected NoProviderSpec, got a different JobAction variant: {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[tokio::test]
    async fn wait_for_job_connect_returns_true_when_already_connected() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        state.set_job_connected("default", true).await;
        assert!(
            state
                .wait_for_job_connect("default", std::time::Duration::from_millis(10))
                .await
        );
    }

    #[tokio::test]
    async fn wait_for_job_connect_times_out() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        assert!(
            !state
                .wait_for_job_connect("default", std::time::Duration::from_millis(10))
                .await
        );
    }

    #[tokio::test]
    async fn wait_for_job_connect_wakes_on_notify() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        let state2 = state.clone();

        let handle = tokio::spawn(async move {
            state2
                .wait_for_job_connect("default", std::time::Duration::from_secs(5))
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        state.set_job_connected("default", true).await;

        assert!(handle.await.unwrap());
    }

    #[tokio::test]
    async fn set_then_get_provider_returns_spec() {
        let state = test_state();
        state
            .set_provider_spec("anthropic".into(), test_provider_spec())
            .await;
        let p = state.get_provider("anthropic").await.expect("provider");
        assert_eq!(p.format, "anthropic");
        assert_eq!(p.secret.name, "anthropic-key");
    }

    #[tokio::test]
    async fn default_or_alphabetic_first_returns_default_when_registered() {
        let state = test_state();
        state.set_model_spec("aaa".into(), test_model_spec()).await;
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        assert_eq!(
            state.default_or_alphabetic_first().await.as_deref(),
            Some("default")
        );
    }

    #[tokio::test]
    async fn default_or_alphabetic_first_returns_alphabetic_first_when_default_absent() {
        let state = test_state();
        state.set_model_spec("aaa".into(), test_model_spec()).await;
        state.set_model_spec("zzz".into(), test_model_spec()).await;
        assert_eq!(
            state.default_or_alphabetic_first().await.as_deref(),
            Some("aaa")
        );
    }

    #[tokio::test]
    async fn default_or_alphabetic_first_returns_none_when_no_models() {
        let state = test_state();
        assert!(state.default_or_alphabetic_first().await.is_none());
    }

    #[tokio::test]
    async fn get_model_spec_returns_some_when_registered() {
        let state = test_state();
        state
            .set_model_spec("default".into(), test_model_spec())
            .await;
        let spec = state.get_model_spec("default").await.expect("spec");
        assert_eq!(spec.model, "claude-sonnet-4-20250514");
    }

    #[tokio::test]
    async fn get_model_spec_returns_none_when_missing() {
        let state = test_state();
        assert!(state.get_model_spec("nope").await.is_none());
    }

    #[tokio::test]
    async fn clear_providers_removes_all() {
        let state = test_state();
        state
            .set_provider_spec("anthropic".into(), test_provider_spec())
            .await;
        state
            .set_provider_spec("mistral".into(), test_provider_spec())
            .await;
        state.clear_providers().await;
        assert!(state.get_provider("anthropic").await.is_none());
        assert!(state.get_provider("mistral").await.is_none());
    }

    /// The controller keys its per-turn cancel token by (workspace,
    /// conversation_id) — `workspace` from the authenticated caller, never the
    /// payload — so a cancel bearing the wrong workspace CANNOT fire another
    /// tenant's turn.
    #[tokio::test]
    async fn fire_cancel_is_a_safe_no_op_for_unknown_and_cross_tenant_keys() {
        let state = test_state();
        state.register_cancel("ws-a", "conv-1").await;

        assert!(
            !state.fire_cancel("ws-a", "conv-unknown").await,
            "an unknown conversation_id must return false, not fire anything"
        );

        assert!(
            !state.fire_cancel("ws-b", "conv-1").await,
            "a cancel from another workspace must not fire ws-a's token"
        );

        let tok = state
            .cancel_token("ws-a", "conv-1")
            .await
            .expect("ws-a's token must still be registered after the no-op attempts");
        assert!(
            !tok.is_cancelled(),
            "the legitimate owner's token must remain un-fired"
        );

        assert!(
            state.fire_cancel("ws-a", "conv-1").await,
            "the correctly-keyed cancel must fire the token and report true"
        );
        assert!(tok.is_cancelled(), "firing must cancel the shared token");

        assert!(
            !state.fire_cancel("ws-a", "conv-1").await,
            "an already-fired key must return false, not error or re-fire"
        );
    }
}
