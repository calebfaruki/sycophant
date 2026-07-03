use crate::crd::{ModelSpec, ProviderSpec};
use hangar_proto::{turn_result_chunk, TurnAssignment, TurnError, TurnResultChunk, TurnRole};
use shared::scheduling::SchedulingConfig;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Notify, RwLock};

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

/// RAII wrapper around an active turn's result sender. Guarantees the
/// consumer's `turn` stream always ends with a terminal event: on `Drop`
/// without a prior `mark_complete()` it `try_send`s a `TurnError`, so any
/// teardown path that drops the `ActiveTurn` without going through
/// `stream_turn_result` — notably the keepalive reap of a worker that
/// connected but never streamed a result — still unblocks the transponder
/// instead of leaving it awaiting forever. `Drop` is synchronous, hence
/// `try_send`; the 64-slot result channel has room for one terminal chunk,
/// and a bare drop on a full channel still ends the stream.
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

    /// Mark the turn as completed by `stream_turn_result` (a successful
    /// Complete, or an explicit error it already sent), so `Drop` does not
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

pub struct ControllerState {
    models: RwLock<HashMap<String, Arc<ModelSlot>>>,
    providers: RwLock<HashMap<String, ProviderSpec>>,
    kube_client: Option<kube::Client>,
    namespace: String,
    controller_addr: String,
    llm_job_image: String,
    scheduling: SchedulingConfig,
}

impl ControllerState {
    pub fn new(
        kube_client: Option<kube::Client>,
        namespace: String,
        controller_addr: String,
        llm_job_image: String,
        scheduling: SchedulingConfig,
    ) -> Self {
        Self {
            models: RwLock::new(HashMap::new()),
            providers: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            controller_addr,
            llm_job_image,
            scheduling,
        }
    }

    pub fn llm_job_image(&self) -> &str {
        &self.llm_job_image
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

    /// Unconditionally take the active turn for `model`, regardless of
    /// owner. Used by teardown paths (the keepalive reap, and the Job
    /// watch) which act on behalf of the cluster, not a workspace caller:
    /// dropping the returned `ActiveTurn` fires its `TurnResultGuard`,
    /// emitting a terminal `TurnError` so the parked transponder stream
    /// ends. Returns `None` when no turn is loaded (e.g. the worker never
    /// pulled the assignment, or already streamed its result).
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
        ControllerState::new(
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

    // ---- TurnResultGuard / take_active_turn (reaper-terminal) tests ----

    fn chunk_error_code(chunk: &TurnResultChunk) -> Option<i32> {
        match &chunk.chunk {
            Some(turn_result_chunk::Chunk::Error(e)) => Some(e.code),
            _ => None,
        }
    }

    #[tokio::test]
    async fn turn_result_guard_drop_emits_terminal_error() {
        // Mutant: drop the `try_send` in Drop → the receiver observes a
        // closed channel with no terminal, and any consumer parked on this
        // stream hangs. This assertion catches that.
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
        // Mutant: ignore `completed` in Drop → a spurious TurnError follows
        // a successful completion. Here recv must see channel-close (None),
        // not an Error chunk.
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
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
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
        // The reap path in miniature: take the turn out of the slot and
        // drop it; the guard fires the terminal so the parked stream ends.
        // Mutant: make `take_active_turn` clone-without-taking → the slot
        // keeps the guard alive and this recv hangs (timeout → failure).
        let state = make_state();
        state.set_model_spec("default".into(), test_spec()).await;
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
