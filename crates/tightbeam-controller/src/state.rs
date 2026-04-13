use crate::conversation::ConversationLog;
use crate::crd::TightbeamModelSpec;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tightbeam_proto::{ChannelOutbound, InboundMessage, TurnAssignment, TurnResultChunk};
use tokio::sync::{broadcast, mpsc, Mutex, Notify, RwLock};

pub struct PendingTurn {
    pub assignment: TurnAssignment,
    pub result_tx: mpsc::Sender<TurnResultChunk>,
    pub workspace: String,
    pub reply_channel: Option<String>,
}

pub enum JobAction {
    AlreadyConnected,
    NoKubeClient,
    NoModelSpec,
    Create(TightbeamModelSpec),
}

pub struct ActiveTurn {
    pub result_tx: mpsc::Sender<TurnResultChunk>,
    pub workspace: String,
    pub reply_channel: Option<String>,
}

struct ModelSlot {
    spec: TightbeamModelSpec,
    pending_tx: mpsc::Sender<PendingTurn>,
    pending_rx: Mutex<mpsc::Receiver<PendingTurn>>,
    active_turn: Mutex<Option<ActiveTurn>>,
    job_connected: Mutex<bool>,
    job_notify: Notify,
}

impl ModelSlot {
    fn new(spec: TightbeamModelSpec) -> Self {
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
    pub conversation: RwLock<ConversationLog>,
    subscriber_tx: broadcast::Sender<InboundMessage>,
}

impl WorkspaceState {
    fn new(conversation: ConversationLog) -> Self {
        let (subscriber_tx, _) = broadcast::channel(16);
        Self {
            conversation: RwLock::new(conversation),
            subscriber_tx,
        }
    }
}

pub struct ControllerState {
    workspaces: RwLock<HashMap<String, Arc<WorkspaceState>>>,
    models: RwLock<HashMap<String, Arc<ModelSlot>>>,
    channels: RwLock<HashMap<String, mpsc::Sender<ChannelOutbound>>>,
    kube_client: Option<kube::Client>,
    namespace: String,
    controller_addr: String,
    llm_job_image: String,
    log_dir: PathBuf,
}

impl ControllerState {
    pub fn new(
        workspace_convs: HashMap<String, ConversationLog>,
        log_dir: PathBuf,
        kube_client: Option<kube::Client>,
        namespace: String,
        controller_addr: String,
        llm_job_image: String,
    ) -> Self {
        let workspaces: HashMap<String, Arc<WorkspaceState>> = workspace_convs
            .into_iter()
            .map(|(name, conv)| (name, Arc::new(WorkspaceState::new(conv))))
            .collect();
        Self {
            workspaces: RwLock::new(workspaces),
            models: RwLock::new(HashMap::new()),
            channels: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            controller_addr,
            llm_job_image,
            log_dir,
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
                let ws_dir = self.log_dir.join(name);
                let conv = ConversationLog::new(&ws_dir);
                Arc::new(WorkspaceState::new(conv))
            })
            .clone()
    }

    pub async fn subscribe(&self, workspace: &str) -> Option<broadcast::Receiver<InboundMessage>> {
        let workspaces = self.workspaces.read().await;
        workspaces
            .get(workspace)
            .map(|ws| ws.subscriber_tx.subscribe())
    }

    pub async fn subscribe_or_create(
        &self,
        workspace: &str,
    ) -> broadcast::Receiver<InboundMessage> {
        let ws = self.get_or_create_workspace(workspace).await;
        ws.subscriber_tx.subscribe()
    }

    pub async fn notify_subscriber(&self, workspace: &str, message: InboundMessage) {
        let workspaces = self.workspaces.read().await;
        if let Some(ws) = workspaces.get(workspace) {
            let _ = ws.subscriber_tx.send(message);
        }
    }

    pub async fn register_channel(&self, key: String, tx: mpsc::Sender<ChannelOutbound>) {
        self.channels.write().await.insert(key, tx);
    }

    pub async fn unregister_channel(&self, key: &str) {
        self.channels.write().await.remove(key);
    }

    pub async fn send_to_channel(&self, key: &str, msg: ChannelOutbound) -> bool {
        let channels = self.channels.read().await;
        if let Some(tx) = channels.get(key) {
            tx.send(msg).await.is_ok()
        } else {
            false
        }
    }

    pub async fn set_model_spec(&self, name: String, spec: TightbeamModelSpec) {
        let mut models = self.models.write().await;
        models.insert(name, Arc::new(ModelSlot::new(spec)));
    }

    pub async fn remove_model(&self, name: &str) {
        self.models.write().await.remove(name);
    }

    pub async fn clear_models(&self) {
        self.models.write().await.clear();
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
        if self.kube_client.is_none() {
            return JobAction::NoKubeClient;
        }
        JobAction::Create(slot.spec.clone())
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

    pub async fn set_active_turn(
        &self,
        model: &str,
        workspace: String,
        reply_channel: Option<String>,
        tx: mpsc::Sender<TurnResultChunk>,
    ) {
        if let Some(slot) = self.get_slot(model).await {
            tracing::info!(model = %model, "set_active_turn");
            *slot.active_turn.lock().await = Some(ActiveTurn {
                result_tx: tx,
                workspace,
                reply_channel,
            });
        }
    }

    pub async fn take_active_turn(&self, model: &str) -> Option<ActiveTurn> {
        let slot = self.get_slot(model).await?;
        let result = slot.active_turn.lock().await.take();
        tracing::info!(model = %model, "take_active_turn: found={}", result.is_some());
        result
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ConversationLog;

    fn make_state() -> ControllerState {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_dir = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let mut workspace_convs = HashMap::new();
        workspace_convs.insert(
            "default".to_string(),
            ConversationLog::new(&log_dir.join("default")),
        );
        ControllerState::new(
            workspace_convs,
            log_dir,
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
        )
    }

    fn test_spec() -> TightbeamModelSpec {
        TightbeamModelSpec {
            format: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            thinking: None,
            secret: None,
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
            },
            result_tx,
            workspace: "default".into(),
            reply_channel: None,
        };

        let state_clone = state.clone();
        let handle = tokio::spawn(async move { state_clone.wait_for_turn("default").await });

        state.enqueue_turn("default", pending).await.unwrap();
        let received = handle.await.unwrap().unwrap();
        assert_eq!(received.assignment.system, Some("test".into()));
    }

    #[tokio::test]
    async fn take_active_turn_returns_none_when_empty() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        assert!(state.take_active_turn("default").await.is_none());
    }

    #[tokio::test]
    async fn set_then_take_active_turn() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        let (tx, _rx) = mpsc::channel::<TurnResultChunk>(1);

        state
            .set_active_turn("default", "ws1".into(), None, tx)
            .await;
        let turn = state.take_active_turn("default").await;
        assert!(turn.is_some());
        assert_eq!(turn.unwrap().workspace, "ws1");
        assert!(
            state.take_active_turn("default").await.is_none(),
            "second take should return None"
        );
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
        let conv = ws.conversation.read().await;
        assert!(conv.is_empty());
    }

    #[tokio::test]
    async fn get_or_create_workspace_returns_existing() {
        let state = make_state();
        let ws1 = state.get_or_create_workspace("test-ws").await;
        let ws2 = state.get_or_create_workspace("test-ws").await;
        assert!(Arc::ptr_eq(&ws1, &ws2));
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

        let msg = InboundMessage {
            content: vec![],
            sender: "test".into(),
        };
        state.notify_subscriber("ws-a", msg).await;

        let received = rx.try_recv().unwrap();
        assert_eq!(received.sender, "test");
    }

    #[tokio::test]
    async fn notify_subscriber_does_not_leak_to_other_workspace() {
        let state = make_state();
        let _ws_a = state.get_or_create_workspace("ws-a").await;
        let _ws_b = state.get_or_create_workspace("ws-b").await;
        let mut rx_a = state.subscribe("ws-a").await.unwrap();
        let mut rx_b = state.subscribe("ws-b").await.unwrap();

        let msg = InboundMessage {
            content: vec![],
            sender: "test".into(),
        };
        state.notify_subscriber("ws-a", msg).await;

        assert!(rx_a.try_recv().is_ok(), "ws-a should receive the message");
        assert!(rx_b.try_recv().is_err(), "ws-b should NOT receive the message");
    }

    #[tokio::test]
    async fn register_channel_and_send() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel::<ChannelOutbound>(1);
        state.register_channel("ch-1".into(), tx).await;

        let outbound = ChannelOutbound { command: None };
        assert!(state.send_to_channel("ch-1", outbound).await);

        let received = rx.recv().await;
        assert!(received.is_some());
    }

    #[tokio::test]
    async fn send_to_channel_does_not_leak_to_other_channel() {
        let state = make_state();
        let (tx_a, mut rx_a) = mpsc::channel::<ChannelOutbound>(1);
        let (tx_b, mut rx_b) = mpsc::channel::<ChannelOutbound>(1);
        state.register_channel("ch-a".into(), tx_a).await;
        state.register_channel("ch-b".into(), tx_b).await;

        let outbound = ChannelOutbound { command: None };
        assert!(state.send_to_channel("ch-a", outbound).await);

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
        state.register_channel("ch-1".into(), tx).await;
        state.unregister_channel("ch-1").await;

        let outbound = ChannelOutbound { command: None };
        assert!(!state.send_to_channel("ch-1", outbound).await);
    }
}
