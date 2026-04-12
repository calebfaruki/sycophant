use crate::conversation::ConversationLog;
use crate::crd::TightbeamModelSpec;
use std::collections::HashMap;
use std::sync::Arc;
use tightbeam_proto::{
    channel_outbound, ChannelOutbound, ChannelSend, ContentBlock, InboundMessage, TurnAssignment,
    TurnResultChunk,
};
use tokio::sync::{broadcast, mpsc, Mutex, Notify, RwLock};

pub struct PendingTurn {
    pub assignment: TurnAssignment,
    pub result_tx: mpsc::Sender<TurnResultChunk>,
}

pub enum JobAction {
    AlreadyConnected,
    NoKubeClient,
    NoModelSpec,
    Create(TightbeamModelSpec),
}

struct ModelSlot {
    spec: TightbeamModelSpec,
    pending_tx: mpsc::Sender<PendingTurn>,
    pending_rx: Mutex<mpsc::Receiver<PendingTurn>>,
    active_result_tx: Mutex<Option<mpsc::Sender<TurnResultChunk>>>,
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
            active_result_tx: Mutex::new(None),
            job_connected: Mutex::new(false),
            job_notify: Notify::new(),
        }
    }
}

pub struct ControllerState {
    pub conversation: Arc<RwLock<ConversationLog>>,
    models: RwLock<HashMap<String, Arc<ModelSlot>>>,
    active_model: Mutex<Option<String>>,
    channel_tx: Mutex<Option<mpsc::Sender<ChannelOutbound>>>,
    subscriber_tx: broadcast::Sender<InboundMessage>,
    kube_client: Option<kube::Client>,
    namespace: String,
    controller_addr: String,
    llm_job_image: String,
}

impl ControllerState {
    pub fn new(
        conversation: ConversationLog,
        kube_client: Option<kube::Client>,
        namespace: String,
        controller_addr: String,
        llm_job_image: String,
    ) -> Self {
        let (subscriber_tx, _) = broadcast::channel(16);
        Self {
            conversation: Arc::new(RwLock::new(conversation)),
            models: RwLock::new(HashMap::new()),
            active_model: Mutex::new(None),
            channel_tx: Mutex::new(None),
            subscriber_tx,
            kube_client,
            namespace,
            controller_addr,
            llm_job_image,
        }
    }

    pub fn llm_job_image(&self) -> &str {
        &self.llm_job_image
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

    pub async fn set_active_result_tx(&self, model: &str, tx: mpsc::Sender<TurnResultChunk>) {
        if let Some(slot) = self.get_slot(model).await {
            tracing::info!(model = %model, "set_active_result_tx");
            *slot.active_result_tx.lock().await = Some(tx);
            *self.active_model.lock().await = Some(model.to_string());
        }
    }

    pub async fn take_active_result_tx_any(&self) -> Option<mpsc::Sender<TurnResultChunk>> {
        let model = self.active_model.lock().await.take()?;
        let slot = self.get_slot(&model).await?;
        let result = slot.active_result_tx.lock().await.take();
        tracing::info!(model = %model, "take_active_result_tx: found={}", result.is_some());
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

    pub fn subscribe(&self) -> broadcast::Receiver<InboundMessage> {
        self.subscriber_tx.subscribe()
    }

    pub fn notify_subscriber(&self, message: InboundMessage) {
        let _ = self.subscriber_tx.send(message);
    }

    pub async fn set_channel_tx(&self, tx: mpsc::Sender<ChannelOutbound>) {
        *self.channel_tx.lock().await = Some(tx);
    }

    pub async fn take_channel_tx(&self) -> Option<mpsc::Sender<ChannelOutbound>> {
        self.channel_tx.lock().await.take()
    }

    pub async fn send_channel_response(&self, content: Vec<ContentBlock>) -> bool {
        let tx = self.channel_tx.lock().await.take();
        if let Some(tx) = tx {
            let outbound = ChannelOutbound {
                command: Some(channel_outbound::Command::SendMessage(ChannelSend {
                    content,
                })),
            };
            let result = tx.send(outbound).await.is_ok();
            result
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ConversationLog;

    fn make_state() -> ControllerState {
        let tmp = tempfile::TempDir::new().unwrap();
        let conv = ConversationLog::new(tmp.path());
        std::mem::forget(tmp);
        ControllerState::new(
            conv,
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
        };

        let state_clone = state.clone();
        let handle = tokio::spawn(async move { state_clone.wait_for_turn("default").await });

        state.enqueue_turn("default", pending).await.unwrap();
        let received = handle.await.unwrap().unwrap();
        assert_eq!(received.assignment.system, Some("test".into()));
    }

    #[tokio::test]
    async fn take_active_result_tx_returns_none_when_empty() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        assert!(state.take_active_result_tx_any().await.is_none());
    }

    #[tokio::test]
    async fn set_then_take_active_result_tx() {
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
        let (tx, _rx) = mpsc::channel::<TurnResultChunk>(1);

        state.set_active_result_tx("default", tx).await;
        assert!(state.take_active_result_tx_any().await.is_some());
        assert!(
            state.take_active_result_tx_any().await.is_none(),
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
    async fn channel_tx_starts_none() {
        let state = make_state();
        assert!(!state.send_channel_response(vec![]).await);
    }

    #[tokio::test]
    async fn channel_tx_set_and_send() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel::<ChannelOutbound>(1);
        state.set_channel_tx(tx).await;

        let content = vec![ContentBlock {
            block: Some(tightbeam_proto::content_block::Block::Text(
                tightbeam_proto::TextBlock {
                    text: "hello".into(),
                },
            )),
        }];
        assert!(state.send_channel_response(content).await);

        let msg = rx.recv().await.unwrap();
        match msg.command {
            Some(channel_outbound::Command::SendMessage(send)) => {
                assert_eq!(send.content.len(), 1);
            }
            _ => panic!("expected SendMessage"),
        }

        assert!(
            !state.send_channel_response(vec![]).await,
            "tx should be consumed after first send"
        );
    }

    #[tokio::test]
    async fn channel_tx_take_clears() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel::<ChannelOutbound>(1);
        state.set_channel_tx(tx).await;
        assert!(state.take_channel_tx().await.is_some());
        assert!(!state.send_channel_response(vec![]).await);
    }
}
