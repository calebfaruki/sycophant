use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use proto_common::tool_result_frame::Frame;
use proto_common::{ToolComplete, ToolOutcome, ToolResultFrame};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use shared::scheduling::SchedulingConfig;
use tokio::sync::{mpsc, watch, Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::warn;

pub use crate::config::PromptConfig;
use crate::config::ToolsetEntry;
use crate::registry::ArgDecl;
use toolset_proto::{turn_result_chunk, TurnAssignment, TurnError, TurnResultChunk, TurnRole};

/// Bound on a tool call's in-flight frame channel, and on a turn's result
/// chunk channel. The tool job client-streams its output into it; the harness's
/// stream drains it.
pub const RESULT_CHANNEL_CAPACITY: usize = 64;

// =========================================================================
// Tool dispatch: toolset bindings, tool registry, pending calls, active Jobs
// =========================================================================

/// The projected ServiceAccount token mount every tool job depends on.
const SA_TOKEN_MOUNT_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// The image's dispatch entrypoint directory.
const DISPATCH_MOUNT_PATH: &str = "/etc/toolset";

/// One operator-approved credential, scoped to one (workspace, toolset) pair.
///
/// `secret` names the Kubernetes Secret carrying it. `path` is where the
/// credential file lands, defaulting to `GRANT_CREDENTIAL_PATH`. `egress` names
/// the one domain the chart opens for it; a grant declaring none mounts its
/// secret and opens nothing.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawGrant")]
pub struct Grant {
    pub secret: String,
    pub path: Option<String>,
    pub egress: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrant {
    secret: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    egress: Option<String>,
}

impl TryFrom<RawGrant> for Grant {
    type Error = String;

    fn try_from(raw: RawGrant) -> Result<Self, Self::Error> {
        if raw.secret.is_empty() {
            return Err("a grant names exactly one Secret, so `secret` must not be empty".into());
        }
        if let Some(path) = &raw.path {
            if !path.starts_with('/') {
                return Err(format!(
                    "grant `path` must be an absolute mount target, got {path:?}"
                ));
            }
            let reserved = path == SA_TOKEN_MOUNT_PATH
                || path == DISPATCH_MOUNT_PATH
                || path.starts_with(&format!("{DISPATCH_MOUNT_PATH}/"))
                || path == crate::WORKSPACE_MOUNT_PATH;
            if reserved {
                return Err(format!(
                    "grant `path` {path} is a reserved mount the tool job already depends on"
                ));
            }
        }
        Ok(Grant {
            secret: raw.secret,
            path: raw.path,
            egress: raw.egress,
        })
    }
}

/// One item of a workspace's toolset list: a bare toolset name, or a named
/// entry carrying a grant menu. Both bind the same toolset by name.
#[derive(Clone, Debug)]
pub enum BindingEntry {
    Bare(String),
    Granted {
        name: String,
        grants: BTreeMap<String, Grant>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrantedEntry {
    name: String,
    grants: BTreeMap<String, Grant>,
}

/// A YAML string is a bare entry and a mapping is a grant-bearing one. Written
/// by hand rather than derived `untagged` so a malformed grant reports the key
/// that is wrong instead of "matched no variant".
impl<'de> Deserialize<'de> for BindingEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match serde_yaml::Value::deserialize(deserializer)? {
            serde_yaml::Value::String(name) => Ok(BindingEntry::Bare(name)),
            other => {
                let entry: RawGrantedEntry =
                    serde_yaml::from_value(other).map_err(D::Error::custom)?;
                Ok(BindingEntry::Granted {
                    name: entry.name,
                    grants: entry.grants,
                })
            }
        }
    }
}

impl BindingEntry {
    /// The bound toolset name in either form.
    pub fn name(&self) -> &str {
        match self {
            BindingEntry::Bare(name) => name,
            BindingEntry::Granted { name, .. } => name,
        }
    }

    /// The entry's grant menu, or `None` for a bare entry.
    pub fn grants(&self) -> Option<&BTreeMap<String, Grant>> {
        match self {
            BindingEntry::Bare(_) => None,
            BindingEntry::Granted { grants, .. } => Some(grants),
        }
    }
}

#[derive(Clone)]
pub struct WorkspaceBindings {
    map: HashMap<String, Vec<BindingEntry>>,
}

impl WorkspaceBindings {
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read bindings file {path}: {e}"))?;
        let map: HashMap<String, Vec<BindingEntry>> = serde_yaml::from_str(&content)
            .map_err(|e| format!("failed to parse bindings YAML: {e}"))?;
        Ok(Self { map })
    }

    pub fn empty() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn from_map(map: HashMap<String, Vec<String>>) -> Self {
        Self {
            map: map
                .into_iter()
                .map(|(ws, toolsets)| (ws, toolsets.into_iter().map(BindingEntry::Bare).collect()))
                .collect(),
        }
    }

    pub fn toolsets_for(&self, workspace: &str) -> &[BindingEntry] {
        self.map.get(workspace).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn has_toolset(&self, workspace: &str, toolset: &str) -> bool {
        self.toolsets_for(workspace)
            .iter()
            .any(|c| c.name() == toolset)
    }

    /// The grant menu bound for this (workspace, toolset) pair. A bare entry
    /// carries no menu, so nothing is selectable against it.
    pub fn grants_for(&self, workspace: &str, toolset: &str) -> Option<&BTreeMap<String, Grant>> {
        self.toolsets_for(workspace)
            .iter()
            .find(|c| c.name() == toolset)
            .and_then(|c| c.grants())
    }

    /// Workspaces bound to `toolset`, in a stable order. The discovery Job runs
    /// under one such workspace's ServiceAccount so its projected token is
    /// mintable; the report it sends is workspace-independent.
    pub fn workspaces_for_toolset(&self, toolset: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .map
            .iter()
            .filter(|(_, toolsets)| toolsets.iter().any(|t| t.name() == toolset))
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

/// The operator-authored toolset config, read once at startup from a
/// chart-rendered ConfigMap. There is no watch: a config change rolls the
/// controller.
#[derive(Clone, Default)]
pub struct ToolsetConfig {
    map: HashMap<String, ToolsetEntry>,
}

impl ToolsetConfig {
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read toolset config file {path}: {e}"))?;
        let map: HashMap<String, ToolsetEntry> = serde_yaml::from_str(&content)
            .map_err(|e| format!("failed to parse toolset config YAML: {e}"))?;
        Ok(Self { map })
    }

    pub fn empty() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn from_map(map: HashMap<String, ToolsetEntry>) -> Self {
        Self { map }
    }

    pub fn get(&self, name: &str) -> Option<&ToolsetEntry> {
        self.map.get(name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut out: Vec<String> = self.map.keys().cloned().collect();
        out.sort();
        out
    }

    pub fn entries(&self) -> impl Iterator<Item = (&String, &ToolsetEntry)> {
        self.map.iter()
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
/// having been forwarded — a tool Job reaped or vanished mid-stream —
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
    /// The call id that spawned the job, which the runtime presents back as
    /// `GetToolCallRequest.job_id`. Empty on a record adopted at reconcile: the
    /// controller did not spawn that job and cannot name it, so the record is
    /// not attachable and every `GetToolCall` against it is refused.
    pub job_id: String,
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
    /// System prompt the prompt job will receive for this turn.
    pub system_prompt: Option<String>,
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
/// `stream_turn_result` — notably the keepalive reap of a tool job that
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
                message: "turn terminated without completion (tool job reaped or vanished)"
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

/// Readiness of a profile's prompt job. One field, not a flag plus a name:
/// the ready deadline and the job's connect both transition it under the same
/// lock, so exactly one of them wins.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PromptJobState {
    /// No job to serve this profile. A `GetTurn` here is refused: a job whose
    /// readyTimeout already fired cannot re-register itself.
    #[default]
    Idle,
    /// The job was created but has not yet asked for work.
    Launching { job_name: String },
    /// The job connected and asked for work.
    Connected { job_name: String },
}

impl PromptJobState {
    fn job_name(&self) -> Option<&str> {
        match self {
            PromptJobState::Idle => None,
            PromptJobState::Launching { job_name } | PromptJobState::Connected { job_name } => {
                Some(job_name)
            }
        }
    }
}

/// Outcome of [`ControllerState::wait_for_job_connect`].
#[derive(Debug, PartialEq)]
pub enum PromptReady {
    /// The job connected and asked for work inside the bound.
    Connected,
    /// The bound expired and the slot was reset to `Idle`. `job_name` names the
    /// job the caller must delete; absent when the slot held none.
    Expired { job_name: Option<String> },
}

/// Per-profile turn-dispatch slot. Keyed by the prompt profile key, created
/// on first use — the config carries no slot state of its own.
struct ModelSlot {
    pending_tx: mpsc::Sender<PendingTurn>,
    pending_rx: Mutex<mpsc::Receiver<PendingTurn>>,
    active_turn: Mutex<Option<ActiveTurn>>,
    job: Mutex<PromptJobState>,
    job_notify: Notify,
    last_activity: Mutex<Instant>,
}

impl ModelSlot {
    fn new() -> Self {
        let (pending_tx, pending_rx) = mpsc::channel(1);
        Self {
            pending_tx,
            pending_rx: Mutex::new(pending_rx),
            active_turn: Mutex::new(None),
            job: Mutex::new(PromptJobState::Idle),
            job_notify: Notify::new(),
            last_activity: Mutex::new(Instant::now()),
        }
    }
}

// =========================================================================
// Unified controller state
// =========================================================================

/// Per-`(workspace, tool_name)` spawn mutex map: held across the tool Job
/// get-probe-create sequence so two concurrent calls for the same tool job
/// cannot both spawn, while distinct workspaces never contend.
type ToolDispatchLocks = HashMap<(String, String), Arc<Mutex<()>>>;

pub struct ControllerState {
    // -- Tool dispatch --
    tools: RwLock<HashMap<String, RegisteredTool>>,
    /// Monotonic counter bumped on every mutation of `tools`. The `WatchTools`
    /// handler holds a `watch::Receiver<u64>` and `.changed().await` to be
    /// woken when the registry changes.
    tools_revision: watch::Sender<u64>,
    toolsets: RwLock<HashMap<String, ToolsetEntry>>,
    /// Pending tool calls keyed by `(workspace, tool_name)`. `workspace` comes
    /// from the authenticated caller, so one workspace's tool job can only dequeue
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
    /// Per-turn cancellation tokens keyed by `(workspace, conversation_id)`.
    /// `workspace` comes from the authenticated caller, never the payload, so
    /// a cancel cannot fire another tenant's turn.
    turn_cancel_tokens: RwLock<HashMap<(String, String), CancellationToken>>,

    // -- Shared --
    kube_client: Option<kube::Client>,
    namespace: String,
    controller_addr: String,
    scheduling: SchedulingConfig,
}

impl ControllerState {
    pub fn new(
        kube_client: Option<kube::Client>,
        namespace: String,
        controller_addr: String,
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
            turn_cancel_tokens: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            controller_addr,
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
            .filter(|(_, tool)| toolsets.iter().any(|c| c.name() == tool.toolset_name))
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

    // ---- Toolset registry ----

    pub async fn get_toolset(&self, name: &str) -> Option<ToolsetEntry> {
        self.toolsets.read().await.get(name).cloned()
    }

    pub async fn set_toolset(&self, name: String, entry: ToolsetEntry) {
        self.toolsets.write().await.insert(name, entry);
    }

    pub async fn toolset_count(&self) -> usize {
        self.toolsets.read().await.len()
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

    /// Take one still-queued call out of the queue, returning whether it was
    /// there. The ready deadline and the job's `dequeue_call` take the same
    /// write lock, so exactly one of them removes the entry: `false` means the
    /// job already claimed the call and the deadline must do nothing.
    pub async fn remove_pending_call(
        &self,
        workspace: &str,
        tool_name: &str,
        call_id: &str,
    ) -> bool {
        let mut pending = self.pending_calls.write().await;
        let Some(calls) = pending.get_mut(&(workspace.to_string(), tool_name.to_string())) else {
            return false;
        };
        let before = calls.len();
        calls.retain(|c| c.call_id != call_id);
        calls.len() != before
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

    /// The workspace that created `call_id`, or `None` once the call finished.
    /// Read by the tool-call ownership gate.
    pub async fn call_owner(&self, call_id: &str) -> Option<String> {
        self.call_id_to_tool
            .read()
            .await
            .get(call_id)
            .map(|(workspace, _)| workspace.clone())
    }

    /// Drains the result channel and reads the `call_id -> (workspace,
    /// tool_name)` shadow entry, returning the tool-job key alongside the sender.
    /// The shadow entry is left in place: it records the call's owner and lives
    /// until the call finishes, which is after the job connects.
    pub async fn take_result_tx(
        &self,
        call_id: &str,
    ) -> Option<(ToolResultGuard, (String, String))> {
        let tx = self.result_txs.write().await.remove(call_id)?;
        let tool_job = self
            .call_id_to_tool
            .read()
            .await
            .get(call_id)
            .cloned()
            .unwrap_or_default();
        Some((tx, tool_job))
    }

    /// Drain every pending result sender whose call is bound to `(workspace,
    /// tool_name)`. Used by the reap path: dropping the returned guards fires
    /// each one's synthetic error terminal, unblocking any harness streaming a
    /// toolset that was torn down. Keying on the workspace too stops one
    /// workspace's tool-job expiry from firing terminals on another workspace's
    /// parked calls for the same tool.
    pub async fn take_result_txs_for_tool_job(
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

    /// Drop bookkeeping for a completed call: its parked receiver, its
    /// cancellation token, and its ownership record. Idempotent; called when
    /// `await_tool_result` returns.
    pub async fn finish_call(&self, call_id: &str) {
        self.result_rxs.write().await.remove(call_id);
        self.call_cancel_tokens.write().await.remove(call_id);
        self.call_id_to_tool.write().await.remove(call_id);
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

    // ---- Turn-dispatch slots ----

    /// Create the turn-dispatch slot for `profile_key` if it has none. Idempotent:
    /// an existing slot keeps its pending queue, active turn, and connection
    /// state, so a second turn on a warm prompt job does not reset it.
    pub async fn ensure_model_slot(&self, profile_key: &str) {
        if self.models.read().await.contains_key(profile_key) {
            return;
        }
        self.models
            .write()
            .await
            .entry(profile_key.to_string())
            .or_insert_with(|| Arc::new(ModelSlot::new()));
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

    /// Readiness of `model`'s prompt job. An unknown profile is `Idle`.
    pub async fn prompt_job_state(&self, model: &str) -> PromptJobState {
        match self.get_slot(model).await {
            Some(slot) => slot.job.lock().await.clone(),
            None => PromptJobState::Idle,
        }
    }

    /// Whether `model`'s prompt job has already connected and is serving turns.
    pub async fn is_job_connected(&self, model: &str) -> bool {
        matches!(
            self.prompt_job_state(model).await,
            PromptJobState::Connected { .. }
        )
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

    /// Record that a prompt job was created for `model` and has yet to ask for
    /// work. A slot already `Connected` is left alone: its job is serving.
    pub async fn set_prompt_job_launching(&self, model: &str, job_name: String) {
        if let Some(slot) = self.get_slot(model).await {
            let mut job = slot.job.lock().await;
            if !matches!(*job, PromptJobState::Connected { .. }) {
                *job = PromptJobState::Launching { job_name };
            }
        }
    }

    /// A prompt job asking for work. `false` on an `Idle` slot — the job's
    /// readyTimeout already fired and it cannot re-register itself.
    pub async fn connect_prompt_job(&self, model: &str) -> bool {
        let Some(slot) = self.get_slot(model).await else {
            return false;
        };
        let mut job = slot.job.lock().await;
        match &*job {
            PromptJobState::Idle => false,
            PromptJobState::Connected { .. } => {
                slot.job_notify.notify_waiters();
                true
            }
            PromptJobState::Launching { job_name } => {
                *job = PromptJobState::Connected {
                    job_name: job_name.clone(),
                };
                slot.job_notify.notify_waiters();
                true
            }
        }
    }

    /// Return `model`'s slot to `Idle`, so the next turn launches a new job.
    /// Used by every teardown path.
    pub async fn reset_prompt_job(&self, model: &str) {
        if let Some(slot) = self.get_slot(model).await {
            *slot.job.lock().await = PromptJobState::Idle;
        }
    }

    /// Wait for `model`'s prompt job to connect, bounded by `timeout`. On
    /// expiry the slot is compare-and-swapped `Launching -> Idle` and the
    /// expired job's name handed back; a connect that got there first leaves
    /// the slot `Connected` and the swap fails, so exactly one of them wins.
    pub async fn wait_for_job_connect(&self, model: &str, timeout: Duration) -> PromptReady {
        let Some(slot) = self.get_slot(model).await else {
            return PromptReady::Expired { job_name: None };
        };
        if matches!(*slot.job.lock().await, PromptJobState::Connected { .. }) {
            return PromptReady::Connected;
        }
        let _ = tokio::time::timeout(timeout, slot.job_notify.notified()).await;

        let mut job = slot.job.lock().await;
        match &*job {
            PromptJobState::Connected { .. } => PromptReady::Connected,
            PromptJobState::Launching { job_name } => {
                let job_name = job_name.clone();
                *job = PromptJobState::Idle;
                PromptReady::Expired {
                    job_name: Some(job_name),
                }
            }
            PromptJobState::Idle => PromptReady::Expired { job_name: None },
        }
    }

    pub async fn bump_model_activity(&self, model: &str) {
        if let Some(slot) = self.get_slot(model).await {
            *slot.last_activity.lock().await = Instant::now();
        }
    }

    pub async fn list_idle_models(&self, idle: Duration, now: Instant) -> Vec<(String, String)> {
        let models = self.models.read().await;
        let mut out = Vec::new();
        for (name, slot) in models.iter() {
            let job_name = match slot.job.lock().await.job_name() {
                Some(n) => n.to_string(),
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

    fn test_toolset() -> ToolsetEntry {
        ToolsetEntry::default()
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
            SchedulingConfig::default(),
        )
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
    async fn toolset_count_reflects_insertions() {
        let state = test_state();
        assert_eq!(state.toolset_count().await, 0);
        state.set_toolset("a".into(), test_toolset()).await;
        state.set_toolset("b".into(), test_toolset()).await;
        assert_eq!(state.toolset_count().await, 2);
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
                job_name: "tool-search-abc".into(),
                job_id: "call-search".into(),
                tool_name: "Search".into(),
                workspace: "ws".into(),
                last_activity: Instant::now(),
                keepalive_seconds: 600,
            })
            .await;

        let got = state.get_active_job("ws", "Search").await.expect("present");
        assert_eq!(got.job_name, "tool-search-abc");
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
                job_name: "tool-shell-abc".into(),
                job_id: "call-shell".into(),
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
    async fn set_take_result_tx_round_trips_tool_job_key() {
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

    /// The ownership record must outlive the job's connect: `take_result_tx`
    /// fires when the job streams its result, and an owner locked out at that
    /// moment could never read its own call.
    #[tokio::test]
    async fn call_owner_survives_job_take_until_finish_call() {
        let state = test_state();
        let (tx, _rx) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        state
            .set_result_tx("call-1".into(), "ws".into(), "Search".into(), tx)
            .await;
        assert_eq!(state.call_owner("call-1").await.as_deref(), Some("ws"));

        let (guard, tool_job) = state.take_result_tx("call-1").await.expect("present");
        assert_eq!(
            tool_job,
            ("ws".to_string(), "Search".to_string()),
            "the tool-job key must still be returned alongside the sender"
        );
        drop(guard);
        assert_eq!(
            state.call_owner("call-1").await.as_deref(),
            Some("ws"),
            "the job connecting must not retire the ownership record"
        );

        state.finish_call("call-1").await;
        assert!(
            state.call_owner("call-1").await.is_none(),
            "the ownership record ends when the call finishes, leaving no entry behind"
        );
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
                job_name: "tool-shell-a".into(),
                job_id: "call-shell-a".into(),
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
    async fn take_result_txs_for_tool_job_isolates_by_workspace() {
        let state = test_state();
        let (tx_a, _rx_a) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        let (tx_b, mut rx_b) = mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        state
            .set_result_tx("call-a".into(), "workspace-a".into(), "shell".into(), tx_a)
            .await;
        state
            .set_result_tx("call-b".into(), "workspace-b".into(), "shell".into(), tx_b)
            .await;

        // Reaping workspace-a's tool job must leave workspace-b's parked call intact.
        drop(
            state
                .take_result_txs_for_tool_job("workspace-a", "shell")
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

    // ---- Turn dispatch ----

    #[tokio::test]
    async fn enqueue_and_wait_delivers() {
        let state = test_state();
        state.ensure_model_slot("default").await;

        let (result_tx, _result_rx) = mpsc::channel(1);
        let pending = PendingTurn {
            assignment: TurnAssignment {
                system: Some("test".into()),
                tools: vec![],
                messages: vec![],
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
        state.ensure_model_slot("default").await;

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
        state.ensure_model_slot("default").await;

        let (pending, _result_rx) = test_pending();
        state.enqueue_turn("default", pending).await.unwrap();

        let drained = state.drain_pending_turns("default").await;
        assert_eq!(drained.len(), 1, "the buffered turn must be drained");
        assert_eq!(drained[0].conversation_id, "test-conv");
    }

    #[tokio::test]
    async fn take_active_turn_if_owned_returns_no_active_turn_when_empty() {
        let state = test_state();
        state.ensure_model_slot("default").await;
        let result = state.take_active_turn_if_owned("default", "ws1").await;
        assert!(matches!(result, Err(TakeTurnError::NoActiveTurn)));
    }

    #[tokio::test]
    async fn set_then_take_active_turn_if_owned() {
        let state = test_state();
        state.ensure_model_slot("default").await;
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
        state.ensure_model_slot("default").await;
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
        state.ensure_model_slot("default").await;
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
        state.ensure_model_slot("default").await;
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
    async fn is_job_connected_is_false_for_an_unknown_profile() {
        let state = test_state();
        assert!(!state.is_job_connected("nonexistent").await);
        assert_eq!(
            state.prompt_job_state("nonexistent").await,
            PromptJobState::Idle
        );
    }

    #[tokio::test]
    async fn prompt_job_state_walks_idle_launching_connected() {
        let state = test_state();
        state.ensure_model_slot("default").await;
        assert_eq!(
            state.prompt_job_state("default").await,
            PromptJobState::Idle
        );
        assert!(!state.is_job_connected("default").await);

        state
            .set_prompt_job_launching("default", "toolset-prompt-default-1".into())
            .await;
        assert_eq!(
            state.prompt_job_state("default").await,
            PromptJobState::Launching {
                job_name: "toolset-prompt-default-1".into()
            }
        );
        assert!(!state.is_job_connected("default").await);

        assert!(state.connect_prompt_job("default").await);
        assert_eq!(
            state.prompt_job_state("default").await,
            PromptJobState::Connected {
                job_name: "toolset-prompt-default-1".into()
            }
        );
        assert!(state.is_job_connected("default").await);
    }

    /// A job whose readyTimeout already fired cannot re-register itself: an
    /// idle slot refuses the connect outright.
    #[tokio::test]
    async fn connect_prompt_job_refuses_an_idle_slot() {
        let state = test_state();
        state.ensure_model_slot("default").await;
        assert!(!state.connect_prompt_job("default").await);
        assert_eq!(
            state.prompt_job_state("default").await,
            PromptJobState::Idle
        );
        assert!(!state.connect_prompt_job("nonexistent").await);
    }

    /// A second turn on a warm profile must not reset the slot: the prompt job is
    /// already connected and re-creating the slot would re-spawn its Job.
    #[tokio::test]
    async fn ensure_model_slot_preserves_an_existing_slot() {
        let state = test_state();
        state.ensure_model_slot("default").await;
        state
            .set_prompt_job_launching("default", "job-1".into())
            .await;
        state.connect_prompt_job("default").await;
        state.ensure_model_slot("default").await;
        assert!(state.is_job_connected("default").await);
    }

    /// A job created while the slot is already serving must not push it back to
    /// Launching: the ready deadline would then delete a live prompt job.
    #[tokio::test]
    async fn set_prompt_job_launching_leaves_a_connected_slot_alone() {
        let state = test_state();
        state.ensure_model_slot("default").await;
        state
            .set_prompt_job_launching("default", "job-1".into())
            .await;
        state.connect_prompt_job("default").await;
        state
            .set_prompt_job_launching("default", "job-2".into())
            .await;
        assert_eq!(
            state.prompt_job_state("default").await,
            PromptJobState::Connected {
                job_name: "job-1".into()
            }
        );
    }

    #[tokio::test]
    async fn wait_for_job_connect_is_connected_when_already_connected() {
        let state = test_state();
        state.ensure_model_slot("default").await;
        state
            .set_prompt_job_launching("default", "job-1".into())
            .await;
        state.connect_prompt_job("default").await;
        assert_eq!(
            state
                .wait_for_job_connect("default", std::time::Duration::from_millis(10))
                .await,
            PromptReady::Connected
        );
    }

    /// Expiry hands back the job to delete and leaves the slot idle, so the next
    /// turn launches a new job instead of reusing the expired one.
    #[tokio::test]
    async fn wait_for_job_connect_expiry_resets_the_slot_and_names_the_job() {
        let state = test_state();
        state.ensure_model_slot("default").await;
        state
            .set_prompt_job_launching("default", "job-1".into())
            .await;
        assert_eq!(
            state
                .wait_for_job_connect("default", std::time::Duration::from_millis(10))
                .await,
            PromptReady::Expired {
                job_name: Some("job-1".into())
            }
        );
        assert_eq!(
            state.prompt_job_state("default").await,
            PromptJobState::Idle
        );
    }

    #[tokio::test]
    async fn wait_for_job_connect_expiry_on_an_idle_slot_names_no_job() {
        let state = test_state();
        state.ensure_model_slot("default").await;
        assert_eq!(
            state
                .wait_for_job_connect("default", std::time::Duration::from_millis(10))
                .await,
            PromptReady::Expired { job_name: None }
        );
        assert_eq!(
            state
                .wait_for_job_connect("nonexistent", std::time::Duration::from_millis(10))
                .await,
            PromptReady::Expired { job_name: None }
        );
    }

    #[tokio::test]
    async fn wait_for_job_connect_wakes_on_connect() {
        let state = test_state();
        state.ensure_model_slot("default").await;
        state
            .set_prompt_job_launching("default", "job-1".into())
            .await;
        let state2 = state.clone();

        let handle = tokio::spawn(async move {
            state2
                .wait_for_job_connect("default", std::time::Duration::from_secs(5))
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        state.connect_prompt_job("default").await;

        assert_eq!(handle.await.unwrap(), PromptReady::Connected);
        assert_eq!(
            state.prompt_job_state("default").await,
            PromptJobState::Connected {
                job_name: "job-1".into()
            }
        );
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
