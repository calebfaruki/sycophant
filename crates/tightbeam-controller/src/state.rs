use crate::conversation::{ConversationLog, ConversationStoreFactory};
use crate::crd::{ModelSpec, ProviderSpec};
use shared::scheduling::SchedulingConfig;
use std::collections::HashMap;
use std::sync::Arc;
use tightbeam_proto::{ChannelOutbound, TurnAssignment, TurnResultChunk, TurnRole, UserMessage};
use tokio::sync::{broadcast, mpsc, Mutex, Notify, RwLock};

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
        }
    }
}

pub struct WorkspaceState {
    /// Workspace name; passed into the factory when constructing per-conv stores.
    name: String,
    factory: Arc<dyn ConversationStoreFactory>,
    conversations: RwLock<HashMap<String, Arc<RwLock<ConversationLog>>>>,
    subscriber_tx: broadcast::Sender<UserMessage>,
}

impl WorkspaceState {
    fn new(name: String, factory: Arc<dyn ConversationStoreFactory>) -> Self {
        let (subscriber_tx, _) = broadcast::channel(16);
        Self {
            name,
            factory,
            conversations: RwLock::new(HashMap::new()),
            subscriber_tx,
        }
    }

    /// Look up an existing conversation. On cache miss, ask the factory for
    /// a `ConversationStore` and rebuild the in-memory log by replaying any
    /// previously persisted events. Errors during replay (corrupt events,
    /// S3 unreachable, etc.) propagate to the caller.
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
        // Re-check after acquiring the write lock (another waiter may have raced us).
        if let Some(c) = convs.get(conv_id) {
            return Ok(c.clone());
        }
        let store = self.factory.make_store(&self.name, conv_id);
        let log = ConversationLog::rebuild(store).await?;
        let arc = Arc::new(RwLock::new(log));
        convs.insert(conv_id.to_string(), arc.clone());
        Ok(arc)
    }

    /// IDs of all conversations the workspace currently knows about.
    /// Order is unspecified.
    pub async fn list_conversation_ids(&self) -> Vec<String> {
        self.conversations.read().await.keys().cloned().collect()
    }
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
        }
    }

    pub fn llm_job_image(&self) -> &str {
        &self.llm_job_image
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
        use crate::conversation::LocalFsFactory;
        // `into_path()` releases the TempDir's drop-time cleanup so the
        // directory survives this function's return. The leak is
        // intentional (test scoped, process-exit cleanup) and explicit at
        // the call site, unlike `mem::forget` which obscures the intent.
        let log_dir = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(log_dir));
        ControllerState::new(
            factory,
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
            SchedulingConfig::default(),
        )
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
        let result = state
            .take_active_turn_if_owned("default", "ws1")
            .await;
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

        let mismatch = state
            .take_active_turn_if_owned("default", "ws-b")
            .await;
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
    async fn list_conversation_ids_returns_known_ids() {
        let state = make_state();
        let ws = state.get_or_create_workspace("ws-list").await;
        let _ = ws.get_or_create_conversation("alpha").await.unwrap();
        let _ = ws.get_or_create_conversation("beta").await.unwrap();
        let mut ids = ws.list_conversation_ids().await;
        ids.sort();
        assert_eq!(ids, vec!["alpha".to_string(), "beta".to_string()]);
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
}
