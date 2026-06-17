use crate::conversation::{ConversationLog, ConversationStoreFactory};
use crate::crd::{ModelSpec, ProviderSpec};
use shared::scheduling::SchedulingConfig;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tightbeam_proto::{
    channel_outbound, ChannelOutbound, ClientResponseError, ServerRequest, TurnAssignment,
    TurnResultChunk, TurnRole, TurnState, TurnStateEvent, UserMessage,
};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, Notify, RwLock};

pub struct PendingTurn {
    pub assignment: TurnAssignment,
    pub result_tx: mpsc::Sender<TurnResultChunk>,
    pub workspace: String,
    pub conversation_id: String,
    pub reply_channel: Option<String>,
    pub role: Option<TurnRole>,
    pub correlation_id: Option<String>,
    /// System prompt the LLM Job will receive for this turn. Carried so we
    /// can hash it onto the assistant log entry for audit.
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

/// Outcome of `State::take_active_turn_if_owned`.
#[derive(Debug)]
pub enum TakeTurnError {
    /// No active turn loaded for this model slot.
    NoActiveTurn,
    /// Active turn exists but the caller's workspace does not own it.
    /// The slot is left intact for the legitimate owner.
    OwnerMismatch { owner: String },
}

pub struct ActiveTurn {
    pub result_tx: mpsc::Sender<TurnResultChunk>,
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
    /// Most recent "still doing useful work" timestamp. Bumped on
    /// `get_turn` arrival, on Job creation, and on successful
    /// `stream_turn_result` Complete chunks. The keepalive sweep
    /// compares `now - last_activity` to `KEEPALIVE_IDLE_SECONDS`.
    last_activity: Mutex<Instant>,
    /// Name of the k8s Job currently spawned for this model. `None`
    /// after the cleanup loop reaps the Job; the next `turn` RPC
    /// observes the empty slot via `check_job_needed` and respawns.
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

/// Per-conversation metadata held in `WorkspaceState`. Distinct from the
/// log itself (which lives in `conversations`) so we can register newly
/// minted conversations before any events have landed.
#[derive(Clone, Debug)]
pub struct ConversationMeta {
    /// Unix epoch milliseconds. 0 = registered but not yet touched.
    /// In-memory only; not persisted. Resets to 0 on controller restart
    /// when the registry is rebuilt from disk.
    pub last_touched_ms: i64,
    /// User-facing name. Defaults to a short id-derived stub at mint
    /// time; mutable via `set_conversation_name`. Persisted as
    /// `meta.json` next to the event log.
    pub name: String,
}

/// First 8 chars of the UUID portion of a conversation id. Used as the
/// default name at mint time so the drawer shows something compact
/// instead of an empty string until the user renames it. Falls back to
/// the whole id when there's no `.` (shouldn't happen for ids minted by
/// `mint_conversation`).
fn default_name_for_conversation(conv_id: &str) -> String {
    let tail = conv_id.rsplit_once('.').map(|(_, t)| t).unwrap_or(conv_id);
    tail.chars().take(8).collect()
}

pub struct WorkspaceState {
    /// Workspace name; passed into the factory when constructing per-conv stores.
    name: String,
    factory: Arc<dyn ConversationStoreFactory>,
    conversations: RwLock<HashMap<String, Arc<RwLock<ConversationLog>>>>,
    /// Conversation registry — every conversation_id that's been minted
    /// or has had any traffic. The flat name list comes from here; the
    /// timestamp drives MRU ordering for `ListConversations`.
    conversation_meta: RwLock<HashMap<String, ConversationMeta>>,
    subscriber_tx: broadcast::Sender<UserMessage>,
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl WorkspaceState {
    fn new(name: String, factory: Arc<dyn ConversationStoreFactory>) -> Self {
        let (subscriber_tx, _) = broadcast::channel(16);
        Self {
            name,
            factory,
            conversations: RwLock::new(HashMap::new()),
            conversation_meta: RwLock::new(HashMap::new()),
            subscriber_tx,
        }
    }

    /// Load an existing conversation log into the in-memory cache. On
    /// cache miss, ask the factory for a `ConversationStore` and rebuild
    /// by replaying persisted events. Does NOT register the id with the
    /// conversation_meta registry — `mint_conversation` is the only path
    /// that grows the registry. A speculative lookup on a never-minted
    /// (or previously-deleted) id returns an empty log without
    /// resurrecting it. Errors during replay propagate to the caller.
    pub async fn get_or_create_conversation(
        &self,
        conv_id: &str,
    ) -> Result<Arc<RwLock<ConversationLog>>, String> {
        {
            let convs = self.conversations.read().await;
            if let Some(c) = convs.get(conv_id) {
                return Ok(c.clone());
            }
        }
        let mut convs = self.conversations.write().await;
        if let Some(c) = convs.get(conv_id) {
            return Ok(c.clone());
        }
        let store = self.factory.make_store(&self.name, conv_id);
        let log = ConversationLog::rebuild(store).await?;
        let arc = Arc::new(RwLock::new(log));
        convs.insert(conv_id.to_string(), arc.clone());
        Ok(arc)
    }

    /// Mint a fresh conversation id, persist its `meta.json` sidecar,
    /// and register it in the meta map. Format:
    /// `<workspace>.<uuid>` — the workspace prefix is the structural
    /// ownership token (see `owns_conversation`).
    ///
    /// Persist-first: the sidecar is written before the registry entry
    /// is inserted, so a partial-failure leaves no ghost id in memory
    /// (and the next caller can retry cleanly).
    pub async fn mint_conversation(&self) -> Result<String, String> {
        let id = format!("{}.{}", self.name, uuid::Uuid::new_v4());
        let name = default_name_for_conversation(&id);
        let store = self.factory.make_store(&self.name, &id);
        store.write_meta(&name).await?;
        self.conversation_meta.write().await.insert(
            id.clone(),
            ConversationMeta {
                last_touched_ms: now_ms(),
                name,
            },
        );
        Ok(id)
    }

    /// Update the user-facing name for an existing conversation. Persists
    /// to the `meta.json` sidecar before mutating the in-memory registry,
    /// so a write failure leaves the displayed name unchanged. Caller is
    /// responsible for the workspace-ownership check (use
    /// `owns_conversation` first) and length validation (the gRPC
    /// `SetConversationName` handler is the single server-side gate for
    /// `MAX_CONVERSATION_NAME_CHARS`).
    pub async fn set_conversation_name(&self, conv_id: &str, new_name: &str) -> Result<(), String> {
        let store = self.factory.make_store(&self.name, conv_id);
        store.write_meta(new_name).await?;
        let mut meta = self.conversation_meta.write().await;
        if let Some(m) = meta.get_mut(conv_id) {
            m.name = new_name.to_string();
        }
        Ok(())
    }

    /// Seed the registry from disk-walk pairs. Inserts each `(id, name)`
    /// only if the id is not already known — a mint racing with the walk
    /// must not be clobbered. `last_touched_ms` starts at 0 since the
    /// disk has no recency signal to recover. Called once per workspace
    /// during startup-walk rebuild.
    pub async fn seed_registry(&self, pairs: Vec<(String, String)>) {
        let mut meta = self.conversation_meta.write().await;
        for (id, name) in pairs {
            meta.entry(id).or_insert(ConversationMeta {
                last_touched_ms: 0,
                name,
            });
        }
    }

    /// Workspace ownership check. Requires both the workspace prefix
    /// AND a live `conversation_meta` registry entry. The in-memory
    /// `conversations` cache is NOT consulted — that map holds rebuilt
    /// or speculatively-loaded logs, and a deleted-but-not-yet-evicted
    /// log must not look "owned".
    pub async fn owns_conversation(&self, conv_id: &str) -> bool {
        if !conv_id.starts_with(&format!("{}.", self.name)) {
            return false;
        }
        self.conversation_meta.read().await.contains_key(conv_id)
    }

    /// Mark the conversation as just-touched. Pulls it to the top of
    /// MRU. No-op on unknown ids — the registry is truth, and a touch
    /// on a never-minted or deleted id must not insert a ghost entry.
    pub async fn touch(&self, conv_id: &str) {
        let mut meta = self.conversation_meta.write().await;
        if let Some(m) = meta.get_mut(conv_id) {
            m.last_touched_ms = now_ms();
        }
    }

    /// Permanently delete a conversation: wipes persisted events FIRST,
    /// then evicts the in-memory cache and registry. On persist failure
    /// the registry is left intact so the caller (and any concurrent
    /// reader) keeps seeing the conversation; the deletion is
    /// retryable. Caller is responsible for the workspace-ownership
    /// check (use `owns_conversation` first).
    pub async fn delete_conversation(&self, conv_id: &str) -> Result<(), String> {
        let store = self.factory.make_store(&self.name, conv_id);
        store.delete_all().await?;
        self.conversation_meta.write().await.remove(conv_id);
        self.conversations.write().await.remove(conv_id);
        Ok(())
    }

    /// `(id, last_touched_ms, name)` triples for every conversation the
    /// workspace currently knows about — the canonical registry view.
    /// Includes freshly minted (no-events-yet) conversations. Returned
    /// in HashMap iteration order — clients render their own sort (the
    /// server contract is "unsorted").
    pub async fn list_conversation_summaries(&self) -> Vec<(String, i64, String)> {
        let meta = self.conversation_meta.read().await;
        meta.iter()
            .map(|(id, m)| (id.clone(), m.last_touched_ms, m.name.clone()))
            .collect()
    }
}

/// Outcome of a server-initiated `ServerRequest` after the client
/// returns a `ClientResponse` (or the wait fails).
#[derive(Debug)]
pub enum ServerRequestOutcome {
    /// Client returned a successful result; payload is `result_json`.
    Result(String),
    /// Client returned a structured error.
    Error(ClientResponseError),
}

/// Reasons a `send_server_request_and_await` call may fail without ever
/// reaching the client OR after dispatching but before a response.
#[derive(Debug)]
pub enum ServerRequestError {
    UnknownChannel,
    UnsupportedMethod,
    SendFailed,
    Timeout,
    Disconnected,
}

/// Server-side state for one registered channel. Created via
/// `mint_channel`; lifetime tied to the originating ChannelReceive /
/// ChannelStream response stream.
pub struct ChannelEntry {
    /// The workspace that minted this channel. ChannelIngest callers
    /// must verify against this binding (PermissionDenied on mismatch).
    pub workspace: String,
    pub tx: mpsc::Sender<ChannelOutbound>,
    /// Free-form, untrusted, log-only label set by the adapter at
    /// registration time. Never used for routing or auth.
    pub adapter_hint: Option<String>,
    /// Cluster-owned turn phase for the channel. `set_and_broadcast_turn_state`
    /// updates this and enqueues a frame under the same lock acquisition so
    /// replay on fresh ChannelReceive cannot race a live transition.
    pub current_state: Mutex<TurnState>,
    /// Outstanding `ServerRequest`s awaiting `ClientResponse` delivery.
    /// Key: request_id (the LLM's tool_call_id, opaque). Value: oneshot
    /// the awaiter parks on. On `ClientResponse` arrival the controller
    /// removes the entry and sends the outcome.
    pub pending_server_requests: Mutex<HashMap<String, oneshot::Sender<ServerRequestOutcome>>>,
    /// Client-advertised tool methods this channel can render. Updated
    /// on every `ChannelIngest` (last-sender-wins). The controller
    /// refuses to dispatch a `ServerRequest` whose method is not in
    /// this set.
    pub supported_methods: Mutex<HashSet<String>>,
}

pub struct ControllerState {
    workspaces: RwLock<HashMap<String, Arc<WorkspaceState>>>,
    models: RwLock<HashMap<String, Arc<ModelSlot>>>,
    providers: RwLock<HashMap<String, ProviderSpec>>,
    /// channel_id (UUID, server-minted) → ChannelEntry.
    channels: RwLock<HashMap<String, ChannelEntry>>,
    kube_client: Option<kube::Client>,
    namespace: String,
    controller_addr: String,
    llm_job_image: String,
    /// Conversation event storage backend. WorkspaceStates clone this Arc
    /// when they're created; per-conversation stores are constructed lazily
    /// on first access via `WorkspaceState::get_or_create_conversation`.
    conversation_factory: Arc<dyn ConversationStoreFactory>,
    scheduling: SchedulingConfig,
    /// Pool of per-workspace transponder clients used to forward external
    /// `WatchTools`/`CallTool` calls. Lazy-constructed per workspace on
    /// first use.
    transponder_clients: Arc<crate::transponder_client::TransponderClientPool>,
}

impl ControllerState {
    pub fn new(
        conversation_factory: Arc<dyn ConversationStoreFactory>,
        kube_client: Option<kube::Client>,
        namespace: String,
        controller_addr: String,
        llm_job_image: String,
        scheduling: SchedulingConfig,
    ) -> Self {
        let transponder_clients = crate::transponder_client::TransponderClientPool::new(&namespace);
        Self {
            workspaces: RwLock::new(HashMap::new()),
            models: RwLock::new(HashMap::new()),
            providers: RwLock::new(HashMap::new()),
            channels: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            controller_addr,
            llm_job_image,
            conversation_factory,
            scheduling,
            transponder_clients,
        }
    }

    /// Borrow the transponder client pool. Handlers use it to dial the
    /// per-workspace transponder when forwarding external tool calls.
    pub fn transponder_clients(&self) -> &Arc<crate::transponder_client::TransponderClientPool> {
        &self.transponder_clients
    }

    pub fn llm_job_image(&self) -> &str {
        &self.llm_job_image
    }

    /// Walk the conversation storage backend once at controller boot and
    /// seed each discovered workspace's in-memory registry with the
    /// `(id, name)` pairs recovered from `meta.json` sidecars on disk.
    /// Per-workspace failures are logged and skipped so a single bad
    /// prefix can't block startup. Returns Err only on a top-level
    /// `list_workspaces` failure.
    pub async fn rebuild_registry_from_disk(&self) -> Result<(), String> {
        let workspaces = self.conversation_factory.list_workspaces().await?;
        for ws_name in workspaces {
            let ws = self.get_or_create_workspace(&ws_name).await;
            match self.conversation_factory.walk_conversations(&ws_name).await {
                Ok(pairs) => {
                    let count = pairs.len();
                    ws.seed_registry(pairs).await;
                    tracing::info!(
                        workspace = %ws_name,
                        seeded = count,
                        "seeded conversation registry from disk",
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        workspace = %ws_name,
                        error = %e,
                        "skipping registry rebuild for workspace",
                    );
                }
            }
        }
        Ok(())
    }

    pub async fn get_or_create_workspace(&self, name: &str) -> Arc<WorkspaceState> {
        {
            let workspaces = self.workspaces.read().await;
            if let Some(ws) = workspaces.get(name) {
                return ws.clone();
            }
        }
        let mut workspaces = self.workspaces.write().await;
        workspaces
            .entry(name.to_string())
            .or_insert_with(|| {
                Arc::new(WorkspaceState::new(
                    name.to_string(),
                    self.conversation_factory.clone(),
                ))
            })
            .clone()
    }

    pub async fn subscribe(&self, workspace: &str) -> Option<broadcast::Receiver<UserMessage>> {
        let workspaces = self.workspaces.read().await;
        workspaces
            .get(workspace)
            .map(|ws| ws.subscriber_tx.subscribe())
    }

    pub async fn subscribe_or_create(&self, workspace: &str) -> broadcast::Receiver<UserMessage> {
        let ws = self.get_or_create_workspace(workspace).await;
        ws.subscriber_tx.subscribe()
    }

    pub async fn notify_subscriber(&self, workspace: &str, message: UserMessage) {
        let workspaces = self.workspaces.read().await;
        if let Some(ws) = workspaces.get(workspace) {
            let _ = ws.subscriber_tx.send(message);
        }
    }

    /// Mint a fresh channel_id (UUID), bind it to the given workspace,
    /// and store the tx for outbound routing. Returns the channel_id;
    /// the caller is responsible for echoing it back to the adapter as
    /// the first frame of the outbound stream (ChannelAck).
    pub async fn mint_channel(
        &self,
        workspace: String,
        adapter_hint: Option<String>,
        tx: mpsc::Sender<ChannelOutbound>,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.channels.write().await.insert(
            id.clone(),
            ChannelEntry {
                workspace,
                tx,
                adapter_hint,
                current_state: Mutex::new(TurnState::Idle),
                pending_server_requests: Mutex::new(HashMap::new()),
                supported_methods: Mutex::new(HashSet::new()),
            },
        );
        id
    }

    pub async fn unregister_channel(&self, channel_id: &str) {
        self.channels.write().await.remove(channel_id);
    }

    /// Return the workspace bound to a channel_id, or None if unknown.
    /// ChannelIngest uses this to enforce the workspace-prefix property:
    /// the caller's verified workspace MUST equal the channel's bound
    /// workspace, otherwise the call is rejected.
    pub async fn channel_workspace(&self, channel_id: &str) -> Option<String> {
        self.channels
            .read()
            .await
            .get(channel_id)
            .map(|entry| entry.workspace.clone())
    }

    pub async fn send_to_channel(&self, channel_id: &str, msg: ChannelOutbound) -> bool {
        let channels = self.channels.read().await;
        if let Some(entry) = channels.get(channel_id) {
            entry.tx.send(msg).await.is_ok()
        } else {
            false
        }
    }

    /// Update the channel's turn phase AND enqueue a matching
    /// `TurnStateEvent` on its outbound mpsc, both under the per-channel
    /// `current_state` mutex. The lock acquisition serialises live
    /// transitions against replay-on-reconnect reads, so a client never
    /// sees a stale state after a fresh ChannelReceive's replay frame.
    /// Returns false if the channel is no longer registered.
    pub async fn set_and_broadcast_turn_state(
        &self,
        channel_id: &str,
        conversation_id: &str,
        state: TurnState,
    ) -> bool {
        let channels = self.channels.read().await;
        let Some(entry) = channels.get(channel_id) else {
            return false;
        };
        let mut guard = entry.current_state.lock().await;
        *guard = state;
        let msg = ChannelOutbound {
            command: Some(channel_outbound::Command::TurnState(TurnStateEvent {
                state: state as i32,
                conversation_id: conversation_id.to_string(),
            })),
        };
        entry.tx.send(msg).await.is_ok()
    }

    /// Replay the channel's current turn phase as a `TurnStateEvent`
    /// without modifying it. Used by `ChannelReceive` after the initial
    /// `ChannelAck` so fresh streams (reconnects, second-device opens)
    /// land in the correct visual state immediately. Emits an empty
    /// `conversation_id` — the channel's current_state is a per-channel
    /// summary, not per-conversation. Clients interpret empty
    /// conversation_id as "no per-conversation update" and update at
    /// most a default/channel-wide indicator.
    pub async fn replay_turn_state(&self, channel_id: &str) -> bool {
        let channels = self.channels.read().await;
        let Some(entry) = channels.get(channel_id) else {
            return false;
        };
        let guard = entry.current_state.lock().await;
        let msg = ChannelOutbound {
            command: Some(channel_outbound::Command::TurnState(TurnStateEvent {
                state: *guard as i32,
                conversation_id: String::new(),
            })),
        };
        entry.tx.send(msg).await.is_ok()
    }

    /// Replace the channel's advertised supported_methods set. Called by
    /// `channel_ingest` on every inbound request because the client may
    /// change devices (each device declares its own renderer set).
    /// Returns false if the channel is no longer registered.
    pub async fn update_supported_methods(&self, channel_id: &str, methods: Vec<String>) -> bool {
        let channels = self.channels.read().await;
        let Some(entry) = channels.get(channel_id) else {
            return false;
        };
        let mut guard = entry.supported_methods.lock().await;
        *guard = methods.into_iter().collect();
        true
    }

    /// Deliver a client's `ClientResponse` to whichever `send_server_request_and_await`
    /// awaiter is parked on the matching request_id. Returns true if a
    /// matching pending request was found and the outcome delivered;
    /// false otherwise (unknown channel, unknown request_id, or awaiter
    /// already dropped — all benign).
    pub async fn deliver_client_response(
        &self,
        channel_id: &str,
        request_id: &str,
        outcome: ServerRequestOutcome,
    ) -> bool {
        let channels = self.channels.read().await;
        let Some(entry) = channels.get(channel_id) else {
            return false;
        };
        let mut pending = entry.pending_server_requests.lock().await;
        let Some(sender) = pending.remove(request_id) else {
            return false;
        };
        sender.send(outcome).is_ok()
    }

    /// Fire-and-forget notification to the client. Empty `request_id` on
    /// the wire signals to the client that no response is expected. Used
    /// by `RevealPath` and any future notification-shaped client tools.
    pub async fn send_server_notification(
        &self,
        channel_id: &str,
        method: &str,
        params_json: String,
    ) -> Result<(), ServerRequestError> {
        let channels = self.channels.read().await;
        let entry = channels
            .get(channel_id)
            .ok_or(ServerRequestError::UnknownChannel)?;
        if !entry.supported_methods.lock().await.contains(method) {
            return Err(ServerRequestError::UnsupportedMethod);
        }
        let msg = ChannelOutbound {
            command: Some(channel_outbound::Command::ServerRequest(ServerRequest {
                request_id: String::new(),
                method: method.to_string(),
                params_json,
            })),
        };
        entry
            .tx
            .send(msg)
            .await
            .map_err(|_| ServerRequestError::SendFailed)
    }

    /// Dispatch a server-initiated request to the client and await the
    /// matching `ClientResponse`. The caller-supplied `request_id` is
    /// the correlation key — pass the LLM's tool_call_id so the agent
    /// loop only needs one identifier.
    ///
    /// Timeout cleanup is best-effort: on `Err(Timeout)` we remove the
    /// pending slot so a late `ClientResponse` doesn't leak memory. A
    /// late response arriving for a removed slot is dropped silently
    /// (deliver_client_response returns false).
    pub async fn send_server_request_and_await(
        self: &Arc<Self>,
        channel_id: &str,
        request_id: &str,
        method: &str,
        params_json: String,
        timeout: Duration,
    ) -> Result<ServerRequestOutcome, ServerRequestError> {
        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        {
            let channels = self.channels.read().await;
            let entry = channels
                .get(channel_id)
                .ok_or(ServerRequestError::UnknownChannel)?;
            if !entry.supported_methods.lock().await.contains(method) {
                return Err(ServerRequestError::UnsupportedMethod);
            }
            entry
                .pending_server_requests
                .lock()
                .await
                .insert(request_id.to_string(), tx_oneshot);
            let msg = ChannelOutbound {
                command: Some(channel_outbound::Command::ServerRequest(ServerRequest {
                    request_id: request_id.to_string(),
                    method: method.to_string(),
                    params_json,
                })),
            };
            if entry.tx.send(msg).await.is_err() {
                entry
                    .pending_server_requests
                    .lock()
                    .await
                    .remove(request_id);
                return Err(ServerRequestError::SendFailed);
            }
        }
        match tokio::time::timeout(timeout, rx_oneshot).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Err(ServerRequestError::Disconnected),
            Err(_) => {
                if let Some(entry) = self.channels.read().await.get(channel_id) {
                    entry
                        .pending_server_requests
                        .lock()
                        .await
                        .remove(request_id);
                }
                Err(ServerRequestError::Timeout)
            }
        }
    }

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

    /// Resolution-time access to a model spec, regardless of job state.
    /// Used by `grpc.rs` to build `params_json` on every turn (the
    /// `JobAction::Create` path only fires on first dispatch).
    pub async fn get_model_spec(&self, name: &str) -> Option<ModelSpec> {
        self.models.read().await.get(name).map(|s| s.spec.clone())
    }

    /// Reserved-name fallback: prefer a model literally named `default`,
    /// otherwise the alphabetic-first registered model. This is the
    /// resolution chain's terminal step when neither frontmatter `model:`
    /// nor a non-empty `params.model` is set.
    pub async fn default_or_alphabetic_first(&self) -> Option<String> {
        let models = self.models.read().await;
        if models.contains_key("default") {
            return Some("default".to_string());
        }
        let mut keys: Vec<&String> = models.keys().collect();
        keys.sort();
        keys.first().map(|s| (*s).clone())
    }

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
        tracing::info!(model = %model, "enqueue_turn: sending pending turn");
        let result = slot
            .pending_tx
            .send(pending)
            .await
            .map_err(|_| "turn queue closed".to_string());
        tracing::info!(model = %model, "enqueue_turn: complete, ok={}", result.is_ok());
        result
    }

    pub async fn wait_for_turn(&self, model: &str) -> Option<PendingTurn> {
        let slot = self.get_slot(model).await?;
        tracing::info!(model = %model, "wait_for_turn: acquiring lock");
        let mut rx = slot.pending_rx.lock().await;
        tracing::info!(model = %model, "wait_for_turn: lock acquired, waiting for message");
        let result = rx.recv().await;
        tracing::info!(model = %model, "wait_for_turn: recv complete, got={}", result.is_some());
        result
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
            tracing::info!(model = %model, "set_active_turn");
            *slot.active_turn.lock().await = Some(ActiveTurn {
                result_tx: tx,
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
    ///
    /// Lock-scoped peek-then-take eliminates TOCTOU: the ownership predicate
    /// and the `take()` happen inside the same mutex critical section. On
    /// `OwnerMismatch` the slot stays intact so the legitimate caller can
    /// still claim it.
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
            Some(_) => {
                let taken = guard.take().expect("guard had Some");
                tracing::info!(model = %model, "take_active_turn_if_owned: taken");
                Ok(taken)
            }
        }
    }

    pub async fn set_job_connected(&self, model: &str, connected: bool) {
        if let Some(slot) = self.get_slot(model).await {
            *slot.job_connected.lock().await = connected;
            if connected {
                slot.job_notify.notify_waiters();
            }
        }
    }

    pub async fn wait_for_job_connect(&self, model: &str, timeout: std::time::Duration) -> bool {
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

    /// Refresh the LLM keepalive idle timer for a model. No-op when the
    /// slot is missing (model removed between turn dispatch and bump).
    pub async fn bump_model_activity(&self, model: &str) {
        if let Some(slot) = self.get_slot(model).await {
            *slot.last_activity.lock().await = Instant::now();
        }
    }

    /// Record (or clear) the active k8s Job name for a model. The
    /// reconcile path at controller startup populates this from k8s;
    /// the dispatch path sets it on successful Job creation; the
    /// cleanup loop clears it after the matching `delete_job`.
    pub async fn set_active_llm_job(&self, model: &str, job_name: Option<String>) {
        if let Some(slot) = self.get_slot(model).await {
            *slot.active_job_name.lock().await = job_name;
        }
    }

    /// Walk every model slot and return `(model_name, job_name)` pairs
    /// for slots where an active Job is registered AND the last activity
    /// timestamp is at least `idle` ago. The keepalive sweep consumes
    /// this to issue `delete_job` calls.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> ControllerState {
        make_state_with_root().0
    }

    /// Variant of `make_state` that also returns the factory's root
    /// directory so tests can assert on-disk side effects (e.g. that
    /// `delete_conversation` actually removed the per-conv directory).
    fn make_state_with_root() -> (ControllerState, std::path::PathBuf) {
        use crate::conversation::LocalFsFactory;
        // `keep()` releases the TempDir's drop-time cleanup so the
        // directory survives this function's return. The leak is
        // intentional (test scoped, process-exit cleanup) and explicit at
        // the call site, unlike `mem::forget` which obscures the intent.
        let log_dir = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> =
            Arc::new(LocalFsFactory::new(log_dir.clone()));
        let state = ControllerState::new(
            factory,
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
            SchedulingConfig::default(),
        );
        (state, log_dir)
    }

    use crate::conversation::test_support::{FailureModes, InjectableFactory};

    fn make_state_with_failing(modes: FailureModes) -> ControllerState {
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(InjectableFactory(modes));
        ControllerState::new(
            factory,
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
            SchedulingConfig::default(),
        )
    }

    fn make_state_with_failing_delete() -> ControllerState {
        make_state_with_failing(FailureModes {
            delete_all: true,
            ..FailureModes::default()
        })
    }

    fn make_state_with_failing_write_meta() -> ControllerState {
        make_state_with_failing(FailureModes {
            write_meta: true,
            ..FailureModes::default()
        })
    }

    fn test_spec() -> ModelSpec {
        ModelSpec {
            provider_ref: crate::crd::ProviderRef {
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
            secret: crate::crd::ProviderSecret {
                name: "anthropic-key".into(),
                key: None,
            },
        }
    }

    #[tokio::test]
    async fn enqueue_and_wait_delivers() {
        let state = Arc::new(make_state());
        state.set_model_spec("default".into(), test_spec()).await;

        let (result_tx, _result_rx) = mpsc::channel(1);
        let pending = PendingTurn {
            assignment: TurnAssignment {
                system: Some("test".into()),
                tools: vec![],
                messages: vec![],
                params_json: None,
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

    #[tokio::test]
    async fn take_active_turn_if_owned_returns_no_active_turn_when_empty() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        let result = state.take_active_turn_if_owned("default", "ws1").await;
        assert!(matches!(result, Err(TakeTurnError::NoActiveTurn)));
    }

    #[tokio::test]
    async fn set_then_take_active_turn_if_owned() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
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
        // Strand-prevention: a wrong-workspace caller must not consume the
        // slot. The legitimate workspace's subsequent call must still
        // return the turn intact.
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
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
            Err(TakeTurnError::OwnerMismatch { ref owner }) => {
                assert_eq!(owner, "ws-a");
            }
            Err(TakeTurnError::NoActiveTurn) => panic!("expected OwnerMismatch, got NoActiveTurn"),
            Ok(_) => panic!("expected OwnerMismatch, got Ok"),
        }

        let legitimate = state
            .take_active_turn_if_owned("default", "ws-a")
            .await
            .expect("legitimate owner can still claim the turn");
        assert_eq!(legitimate.workspace, "ws-a");
    }

    #[tokio::test]
    async fn check_job_needed_no_model_spec() {
        let state = make_state();
        assert!(matches!(
            state.check_job_needed("nonexistent").await,
            JobAction::NoModelSpec
        ));
    }

    #[tokio::test]
    async fn check_job_needed_no_kube_client() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
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
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        state.set_job_connected("default", true).await;
        assert!(matches!(
            state.check_job_needed("default").await,
            JobAction::AlreadyConnected
        ));
    }

    #[tokio::test]
    async fn wait_for_job_connect_returns_true_when_already_connected() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        state.set_job_connected("default", true).await;
        assert!(
            state
                .wait_for_job_connect("default", std::time::Duration::from_millis(10))
                .await
        );
    }

    #[tokio::test]
    async fn wait_for_job_connect_times_out() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        assert!(
            !state
                .wait_for_job_connect("default", std::time::Duration::from_millis(10))
                .await
        );
    }

    #[tokio::test]
    async fn wait_for_job_connect_wakes_on_notify() {
        let state = Arc::new(make_state());
        state.set_model_spec("default".into(), test_spec()).await;
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
    async fn multiple_models_independent() {
        let state = make_state();
        state.set_model_spec("haiku".into(), test_spec()).await;
        state.set_model_spec("sonnet".into(), test_spec()).await;
        state
            .set_provider_spec("anthropic".into(), test_provider_spec())
            .await;

        state.set_job_connected("haiku", true).await;
        assert!(matches!(
            state.check_job_needed("haiku").await,
            JobAction::AlreadyConnected
        ));
        assert!(matches!(
            state.check_job_needed("sonnet").await,
            JobAction::NoKubeClient
        ));
    }

    #[tokio::test]
    async fn get_or_create_workspace_creates_new() {
        let state = make_state();
        let ws = state.get_or_create_workspace("new-workspace").await;
        let conv = ws
            .get_or_create_conversation("test-conv")
            .await
            .expect("default conversation rebuilds (empty dir)");
        assert!(conv.read().await.is_empty());
    }

    #[tokio::test]
    async fn get_or_create_workspace_returns_existing() {
        let state = make_state();
        let ws1 = state.get_or_create_workspace("test-ws").await;
        let ws2 = state.get_or_create_workspace("test-ws").await;
        assert!(Arc::ptr_eq(&ws1, &ws2));
    }

    #[tokio::test]
    async fn workspace_holds_multiple_conversations_keyed_by_conv_id() {
        use tightbeam_providers::types::{content_text, ContentBlock, Message};

        fn text_msg(role: &str, text: &str) -> Message {
            Message {
                role: role.into(),
                content: Some(ContentBlock::text_content(text)),
                tool_calls: None,
                tool_call_id: None,
                is_error: None,
            }
        }

        let state = make_state();
        let ws = state.get_or_create_workspace("ws-multi").await;

        let c1 = ws.get_or_create_conversation("conv-A").await.unwrap();
        let c2 = ws.get_or_create_conversation("conv-B").await.unwrap();

        c1.write()
            .await
            .append(text_msg("user", "in A"))
            .await
            .unwrap();
        c2.write()
            .await
            .append(text_msg("user", "in B"))
            .await
            .unwrap();

        let h1 = c1.read().await.history();
        let h2 = c2.read().await.history();
        assert_eq!(h1.len(), 1);
        assert_eq!(h2.len(), 1);
        assert_eq!(content_text(&h1[0].content), Some("in A"));
        assert_eq!(content_text(&h2[0].content), Some("in B"));
    }

    #[tokio::test]
    async fn get_or_create_conversation_returns_same_arc_for_same_id() {
        let state = make_state();
        let ws = state.get_or_create_workspace("ws-stable").await;
        let a = ws.get_or_create_conversation("conv-X").await.unwrap();
        let b = ws.get_or_create_conversation("conv-X").await.unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn list_conversation_summaries_returns_known_ids() {
        let state = make_state();
        let ws = state.get_or_create_workspace("ws-list").await;
        let a = ws.mint_conversation().await.unwrap();
        let b = ws.mint_conversation().await.unwrap();
        let mut ids: Vec<String> = ws
            .list_conversation_summaries()
            .await
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        ids.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(ids, expected);
    }

    #[tokio::test]
    async fn subscribe_unknown_workspace_returns_none() {
        let state = make_state();
        assert!(state.subscribe("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn notify_subscriber_routes_to_correct_workspace() {
        let state = make_state();
        let _ws = state.get_or_create_workspace("ws-a").await;
        let mut rx = state.subscribe("ws-a").await.unwrap();

        let msg = UserMessage {
            content: vec![],
            sender: "test".into(),
            reply_channel: None,
            conversation_id: String::new(),
        };
        state.notify_subscriber("ws-a", msg).await;

        let received = rx.try_recv().unwrap();
        assert_eq!(received.sender, "test");
    }

    #[tokio::test]
    async fn notify_subscriber_preserves_reply_channel() {
        let state = make_state();
        let _ws = state.get_or_create_workspace("ws-a").await;
        let mut rx = state.subscribe("ws-a").await.unwrap();

        let msg = UserMessage {
            content: vec![],
            sender: "test".into(),
            reply_channel: Some("test-channel".into()),
            conversation_id: String::new(),
        };
        state.notify_subscriber("ws-a", msg).await;

        let received = rx.try_recv().unwrap();
        assert_eq!(
            received.reply_channel.as_deref(),
            Some("test-channel"),
            "reply_channel must be preserved through broadcast"
        );
    }

    #[tokio::test]
    async fn notify_subscriber_does_not_leak_to_other_workspace() {
        let state = make_state();
        let _ws_a = state.get_or_create_workspace("ws-a").await;
        let _ws_b = state.get_or_create_workspace("ws-b").await;
        let mut rx_a = state.subscribe("ws-a").await.unwrap();
        let mut rx_b = state.subscribe("ws-b").await.unwrap();

        let msg = UserMessage {
            content: vec![],
            sender: "test".into(),
            reply_channel: None,
            conversation_id: String::new(),
        };
        state.notify_subscriber("ws-a", msg).await;

        assert!(rx_a.try_recv().is_ok(), "ws-a should receive the message");
        assert!(
            rx_b.try_recv().is_err(),
            "ws-b should NOT receive the message"
        );
    }

    #[tokio::test]
    async fn mint_channel_and_send() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel::<ChannelOutbound>(1);
        let channel_id = state
            .mint_channel("ws-a".into(), Some("test-hint".into()), tx)
            .await;
        assert!(!channel_id.is_empty(), "mint should return a non-empty id");

        let outbound = ChannelOutbound { command: None };
        assert!(state.send_to_channel(&channel_id, outbound).await);

        let received = rx.recv().await;
        assert!(received.is_some());
    }

    #[tokio::test]
    async fn mint_channel_returns_unique_ids() {
        let state = make_state();
        let (tx1, _rx1) = mpsc::channel::<ChannelOutbound>(1);
        let (tx2, _rx2) = mpsc::channel::<ChannelOutbound>(1);
        let id1 = state.mint_channel("ws-a".into(), None, tx1).await;
        let id2 = state.mint_channel("ws-a".into(), None, tx2).await;
        assert_ne!(id1, id2, "each mint must return a unique id");
    }

    #[tokio::test]
    async fn channel_workspace_returns_binding() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel::<ChannelOutbound>(1);
        let channel_id = state.mint_channel("ws-a".into(), None, tx).await;
        assert_eq!(
            state.channel_workspace(&channel_id).await.as_deref(),
            Some("ws-a")
        );
        assert!(
            state.channel_workspace("nonexistent").await.is_none(),
            "unknown channel_id must return None"
        );
    }

    #[tokio::test]
    async fn send_to_channel_does_not_leak_to_other_channel() {
        let state = make_state();
        let (tx_a, mut rx_a) = mpsc::channel::<ChannelOutbound>(1);
        let (tx_b, mut rx_b) = mpsc::channel::<ChannelOutbound>(1);
        let id_a = state.mint_channel("ws-a".into(), None, tx_a).await;
        let _id_b = state.mint_channel("ws-b".into(), None, tx_b).await;

        let outbound = ChannelOutbound { command: None };
        assert!(state.send_to_channel(&id_a, outbound).await);

        assert!(rx_a.try_recv().is_ok(), "ch-a should receive the message");
        assert!(
            rx_b.try_recv().is_err(),
            "ch-b should NOT receive the message"
        );
    }

    #[tokio::test]
    async fn unregister_channel_removes() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel::<ChannelOutbound>(1);
        let channel_id = state.mint_channel("ws-a".into(), None, tx).await;
        state.unregister_channel(&channel_id).await;

        let outbound = ChannelOutbound { command: None };
        assert!(!state.send_to_channel(&channel_id, outbound).await);
        assert!(state.channel_workspace(&channel_id).await.is_none());
    }

    #[tokio::test]
    async fn set_then_get_provider_returns_spec() {
        let state = make_state();
        state
            .set_provider_spec("anthropic".into(), test_provider_spec())
            .await;
        let p = state.get_provider("anthropic").await.expect("provider");
        assert_eq!(p.format, "anthropic");
        assert_eq!(p.secret.name, "anthropic-key");
    }

    #[tokio::test]
    async fn default_or_alphabetic_first_returns_default_when_registered() {
        let state = make_state();
        // Register `aaa` (alphabetically first) and `default`. The reserved
        // name must win regardless of alphabetic ordering.
        state.set_model_spec("aaa".into(), test_spec()).await;
        state.set_model_spec("default".into(), test_spec()).await;
        assert_eq!(
            state.default_or_alphabetic_first().await.as_deref(),
            Some("default")
        );
    }

    #[tokio::test]
    async fn default_or_alphabetic_first_returns_alphabetic_first_when_default_absent() {
        let state = make_state();
        state.set_model_spec("aaa".into(), test_spec()).await;
        state.set_model_spec("zzz".into(), test_spec()).await;
        assert_eq!(
            state.default_or_alphabetic_first().await.as_deref(),
            Some("aaa")
        );
    }

    #[tokio::test]
    async fn default_or_alphabetic_first_returns_none_when_no_models() {
        let state = make_state();
        assert!(state.default_or_alphabetic_first().await.is_none());
    }

    #[tokio::test]
    async fn get_model_spec_returns_some_when_registered() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        let spec = state.get_model_spec("default").await.expect("spec");
        assert_eq!(spec.model, "claude-sonnet-4-20250514");
    }

    #[tokio::test]
    async fn get_model_spec_returns_none_when_missing() {
        let state = make_state();
        assert!(state.get_model_spec("nope").await.is_none());
    }

    #[tokio::test]
    async fn clear_providers_removes_all() {
        let state = make_state();
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

    #[tokio::test]
    async fn check_job_needed_returns_no_provider_spec_when_referenced_provider_missing() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        match state.check_job_needed("default").await {
            JobAction::NoProviderSpec(name) => assert_eq!(name, "anthropic"),
            other => panic!(
                "expected NoProviderSpec, got a different JobAction variant: {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[tokio::test]
    async fn check_job_needed_no_kube_client_returns_after_provider_resolves() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        state
            .set_provider_spec("anthropic".into(), test_provider_spec())
            .await;
        assert!(matches!(
            state.check_job_needed("default").await,
            JobAction::NoKubeClient
        ));
    }

    // ---- TurnState lifecycle tests ----
    //
    // These exercise `set_and_broadcast_turn_state` and `replay_turn_state`
    // directly: state mutation is serialised with the outbound frame, replay
    // is non-mutating, and the FIFO ordering of the channel mpsc means a
    // client sees state events in the order they were emitted.

    fn extract_turn_state(msg: &ChannelOutbound) -> Option<TurnState> {
        match &msg.command {
            Some(channel_outbound::Command::TurnState(event)) => {
                TurnState::try_from(event.state).ok()
            }
            _ => None,
        }
    }

    #[tokio::test]
    async fn mint_channel_defaults_current_state_to_idle() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel::<ChannelOutbound>(4);
        let channel_id = state.mint_channel("ws".into(), None, tx).await;
        // Replay without prior transition must yield IDLE — proves both the
        // default and that replay reads the live mutex.
        assert!(state.replay_turn_state(&channel_id).await);
        let msg = rx.recv().await.expect("replay must enqueue a frame");
        assert_eq!(extract_turn_state(&msg), Some(TurnState::Idle));
    }

    #[tokio::test]
    async fn set_and_broadcast_turn_state_updates_state_and_emits_frame() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel::<ChannelOutbound>(4);
        let channel_id = state.mint_channel("ws".into(), None, tx).await;

        assert!(
            state
                .set_and_broadcast_turn_state(&channel_id, "test-conv", TurnState::Working)
                .await
        );

        let msg = rx.recv().await.expect("frame must be enqueued");
        assert_eq!(extract_turn_state(&msg), Some(TurnState::Working));

        // Replay must observe the updated state, not the IDLE default —
        // proves the mutex update committed before send.
        assert!(state.replay_turn_state(&channel_id).await);
        let replay = rx.recv().await.expect("replay frame");
        assert_eq!(extract_turn_state(&replay), Some(TurnState::Working));
    }

    #[tokio::test]
    async fn set_and_broadcast_turn_state_returns_false_for_unknown_channel() {
        let state = make_state();
        assert!(
            !state
                .set_and_broadcast_turn_state("nonexistent", "test-conv", TurnState::Working)
                .await
        );
        assert!(!state.replay_turn_state("nonexistent").await);
    }

    #[tokio::test]
    async fn turn_state_transitions_arrive_in_fifo_order() {
        // The single mpsc the controller uses means a WORKING-then-IDLE pair
        // (or any other transition sequence) must arrive at the client in
        // the same order. Validates that `set_and_broadcast_turn_state`
        // doesn't accidentally re-order via select / spawn.
        let state = make_state();
        let (tx, mut rx) = mpsc::channel::<ChannelOutbound>(4);
        let channel_id = state.mint_channel("ws".into(), None, tx).await;

        state
            .set_and_broadcast_turn_state(&channel_id, "test-conv", TurnState::Working)
            .await;
        state
            .set_and_broadcast_turn_state(&channel_id, "test-conv", TurnState::Idle)
            .await;

        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        assert_eq!(extract_turn_state(&first), Some(TurnState::Working));
        assert_eq!(extract_turn_state(&second), Some(TurnState::Idle));
    }

    // ---- delete_conversation / registry-as-truth tests ----

    fn text_user(text: &str) -> tightbeam_providers::types::Message {
        use tightbeam_providers::types::{ContentBlock, Message};
        Message {
            role: "user".into(),
            content: Some(ContentBlock::text_content(text)),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
        }
    }

    #[tokio::test]
    async fn delete_conversation_persist_failure_leaves_registry_intact() {
        // If `store.delete_all` fails, the in-memory registry MUST NOT
        // evict the conversation. Otherwise the next request creates an
        // empty log over orphaned events and the deletion is silently
        // partial. Caller sees Err and can retry; until then the
        // conversation is still listed and still owned.
        let state = make_state_with_failing_delete();
        let ws = state.get_or_create_workspace("default").await;
        let conv_id = ws.mint_conversation().await.unwrap();
        let log = ws.get_or_create_conversation(&conv_id).await.unwrap();
        log.write().await.append(text_user("hello")).await.unwrap();

        let result = ws.delete_conversation(&conv_id).await;
        assert!(result.is_err(), "delete_all failure must propagate");

        assert!(
            ws.owns_conversation(&conv_id).await,
            "registry must still own the conversation after a failed persist-delete"
        );
        assert!(
            ws.list_conversation_summaries()
                .await
                .iter()
                .any(|(id, _, _)| id == &conv_id),
            "registry view must still include the conversation"
        );
    }

    #[tokio::test]
    async fn delete_conversation_success_purges_registry_and_disk() {
        let (state, root) = make_state_with_root();
        let ws = state.get_or_create_workspace("default").await;
        let conv_id = ws.mint_conversation().await.unwrap();
        let log = ws.get_or_create_conversation(&conv_id).await.unwrap();
        log.write().await.append(text_user("hello")).await.unwrap();

        let conv_dir = root.join("default").join(&conv_id);
        assert!(
            conv_dir.exists(),
            "append should have created the per-conv directory at {conv_dir:?}"
        );

        ws.delete_conversation(&conv_id)
            .await
            .expect("happy-path delete succeeds");

        assert!(
            !ws.owns_conversation(&conv_id).await,
            "registry must drop the conversation on success"
        );
        assert!(
            !ws.list_conversation_summaries()
                .await
                .iter()
                .any(|(id, _, _)| id == &conv_id),
            "registry view must drop the conversation on success"
        );
        assert!(
            !conv_dir.exists(),
            "per-conv directory must be removed on success"
        );
    }

    #[tokio::test]
    async fn get_or_create_conversation_does_not_register_unknown_id() {
        // Cache miss must rebuild the in-memory log WITHOUT minting a
        // registry entry. The registry is truth; disk is a cache. A
        // speculative get must not resurrect a previously-deleted id.
        let state = make_state();
        let ws = state.get_or_create_workspace("default").await;
        let speculative = "default.never-minted";

        let _ = ws.get_or_create_conversation(speculative).await.unwrap();

        assert!(
            !ws.owns_conversation(speculative).await,
            "get_or_create_conversation must not insert into the registry"
        );
        assert!(
            !ws.list_conversation_summaries()
                .await
                .iter()
                .any(|(id, _, _)| id == speculative),
            "speculative id must not show up in the registry view"
        );
    }

    #[tokio::test]
    async fn touch_unknown_id_is_noop() {
        let state = make_state();
        let ws = state.get_or_create_workspace("default").await;

        let unknown = "default.never-minted";
        ws.touch(unknown).await;
        assert!(
            !ws.owns_conversation(unknown).await,
            "touch on an unknown id must not insert it into the registry"
        );

        let minted = ws.mint_conversation().await.unwrap();
        let before = ws
            .list_conversation_summaries()
            .await
            .into_iter()
            .find(|(id, _, _)| id == &minted)
            .map(|(_, ts, _)| ts)
            .expect("minted conv present");

        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        ws.touch(&minted).await;

        let after = ws
            .list_conversation_summaries()
            .await
            .into_iter()
            .find(|(id, _, _)| id == &minted)
            .map(|(_, ts, _)| ts)
            .expect("minted conv present");

        assert!(
            after >= before,
            "touch on a known id must advance last_touched_ms (before={before}, after={after})"
        );
    }

    #[tokio::test]
    async fn deleted_conversation_does_not_resurrect_via_rebuild() {
        // The user-visible bug: device A deletes, device B opens the
        // same conv_id, on-disk events were nuked so rebuild returns
        // empty, BUT the registry must stay empty too — otherwise the
        // ghost id shows up in ListConversations and `owns_conversation`
        // wrongly returns true on the next access.
        let state = make_state();
        let ws = state.get_or_create_workspace("default").await;
        let conv_id = ws.mint_conversation().await.unwrap();
        let log = ws.get_or_create_conversation(&conv_id).await.unwrap();
        log.write().await.append(text_user("hello")).await.unwrap();

        ws.delete_conversation(&conv_id)
            .await
            .expect("delete succeeds");

        let rebuilt = ws.get_or_create_conversation(&conv_id).await.unwrap();
        assert!(
            rebuilt.read().await.is_empty(),
            "rebuilt log on a deleted conv must be empty"
        );
        assert!(
            !ws.owns_conversation(&conv_id).await,
            "deleted conv must not resurrect into the registry via rebuild"
        );
        assert!(
            !ws.list_conversation_summaries()
                .await
                .iter()
                .any(|(id, _, _)| id == &conv_id),
            "deleted conv must not appear in the registry view after rebuild"
        );
    }

    #[tokio::test]
    async fn mint_persist_failure_leaves_registry_empty() {
        // Mutation target: swap the order in `mint_conversation` so the
        // registry insert happens BEFORE `store.write_meta`. Under the
        // mutant, the failing write_meta still returns Err but the
        // registry already holds the id — and the registry view below
        // comes back non-empty, failing this test.
        let state = make_state_with_failing_write_meta();
        let ws = state.get_or_create_workspace("default").await;
        let err = ws
            .mint_conversation()
            .await
            .expect_err("mint must propagate persist failure rather than return a phantom id");
        assert!(err.contains("write_meta"));
        assert!(
            ws.list_conversation_summaries().await.is_empty(),
            "registry must stay empty when sidecar persist fails"
        );
    }

    #[tokio::test]
    async fn mint_writes_default_name_to_disk_and_registry() {
        let (state, root) = make_state_with_root();
        let ws = state.get_or_create_workspace("default").await;
        let id = ws.mint_conversation().await.unwrap();

        // Default name = first 8 chars of the uuid suffix (the bit after `.`).
        let expected = default_name_for_conversation(&id);
        assert_eq!(expected.chars().count(), 8);

        // Registry holds the same name we wrote.
        let summaries = ws.list_conversation_summaries().await;
        let (_, _, name_in_registry) = summaries
            .into_iter()
            .find(|(rid, _, _)| rid == &id)
            .unwrap();
        assert_eq!(name_in_registry, expected);

        // meta.json on disk holds it too.
        let meta_path = root.join("default").join(&id).join("meta.json");
        let body = std::fs::read_to_string(&meta_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["name"], expected);
    }

    #[tokio::test]
    async fn set_conversation_name_persists_and_survives_rebuild() {
        let (state, _root) = make_state_with_root();
        let ws = state.get_or_create_workspace("default").await;
        let id = ws.mint_conversation().await.unwrap();

        ws.set_conversation_name(&id, "Quarterly review")
            .await
            .unwrap();

        // Registry sees the new name immediately.
        let after = ws
            .list_conversation_summaries()
            .await
            .into_iter()
            .find(|(rid, _, _)| rid == &id)
            .unwrap();
        assert_eq!(after.2, "Quarterly review");

        // Now simulate a controller restart: rebuild the registry from
        // disk and confirm the renamed name survives.
        state.rebuild_registry_from_disk().await.unwrap();
        let after_restart = ws
            .list_conversation_summaries()
            .await
            .into_iter()
            .find(|(rid, _, _)| rid == &id)
            .unwrap();
        assert_eq!(after_restart.2, "Quarterly review");
    }

    #[tokio::test]
    async fn rebuild_registry_from_disk_seeds_minted_conversations() {
        // Mutation target: in `ControllerState::rebuild_registry_from_disk`
        // skip the `ws.seed_registry(pairs)` call. Under the mutant the
        // registry stays empty after rebuild and the assertion below
        // fails.
        let (state, _root) = make_state_with_root();

        // Mint two conversations through the live state so meta.json
        // sidecars exist on disk for the workspace.
        let ws = state.get_or_create_workspace("default").await;
        let id_a = ws.mint_conversation().await.unwrap();
        let id_b = ws.mint_conversation().await.unwrap();
        drop(ws);

        // Drop all in-memory workspace state by reconstructing a fresh
        // `ControllerState` over the same factory.
        let same_factory = state.conversation_factory.clone();
        let fresh = ControllerState::new(
            same_factory,
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
            SchedulingConfig::default(),
        );
        fresh.rebuild_registry_from_disk().await.unwrap();

        let ws = fresh.get_or_create_workspace("default").await;
        let mut ids: Vec<String> = ws
            .list_conversation_summaries()
            .await
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        ids.sort();
        let mut expected = vec![id_a, id_b];
        expected.sort();
        assert_eq!(ids, expected);
    }

    #[tokio::test]
    async fn deleted_conversation_does_not_resurrect_after_rebuild() {
        // The composed-invariant test: delete removes the folder
        // (including meta.json), so the startup walk has nothing to
        // find. Mutation target: change `delete_all` to leave the
        // meta.json sidecar behind — the rebuild would then resurrect
        // the deleted id and this test goes red.
        let (state, _root) = make_state_with_root();

        let ws = state.get_or_create_workspace("default").await;
        let id = ws.mint_conversation().await.unwrap();
        ws.delete_conversation(&id).await.unwrap();
        drop(ws);

        let same_factory = state.conversation_factory.clone();
        let fresh = ControllerState::new(
            same_factory,
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
            SchedulingConfig::default(),
        );
        fresh.rebuild_registry_from_disk().await.unwrap();

        let ws = fresh.get_or_create_workspace("default").await;
        assert!(
            ws.list_conversation_summaries().await.is_empty(),
            "deleted conversation must not be seeded back from disk"
        );
    }
}
