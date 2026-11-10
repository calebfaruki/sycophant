//! In-memory state owned by the internet-facing gateway pod.
//!
//! The gateway does not own the agent's conversation; it lives in the
//! per-agent harness, reached via the `HarnessClientPool`. The gateway owns
//! the *live* surface:
//!
//! - `channel_streams` — server-minted channel_stream_id → outbound mpsc.
//! - `last_turn_state` — per-agent last recorded phase, backing the
//!   `GetTurnState` poll and the replay a newly opened stream gets.
//! - `SubscriberRegistry` — per-agent broadcast bus the harness's
//!   `Subscribe` stream drains; `ChannelStreamIngest` notifies it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use proto_common::{
    channel_stream_outbound, ChannelStreamOutbound, ClientResponseError, Heartbeat, ServerRequest,
    TurnState, TurnStateEvent, UserMessage,
};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use tokio::task::AbortHandle;

use crate::harness_client::HarnessClientPool;
use crate::identity::IdentityTable;

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
    UnknownChannelStream,
    UnsupportedMethod,
    SendFailed,
    Timeout,
    Disconnected,
}

/// Who opened a channel stream. ChannelStreamIngest admits only the same caller: a
/// person-opened stream only its own subject, a channel-opened stream only that
/// channel. A channel-opened stream also keeps the sender handle it was opened
/// for, so every message re-resolves that handle against the live identity
/// table and a removed handle binding cuts an open channel stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelStreamOpener {
    Person,
    Channel { channel: String, sender: String },
}

/// Server-side state for one registered channel stream. Created via
/// `mint_channel_stream`; lifetime tied to the originating ChannelStreamReceive
/// response stream.
pub struct ChannelStreamEntry {
    /// How the channel stream was opened; see [`ChannelStreamOpener`].
    pub opener: ChannelStreamOpener,
    /// The subject that minted this channel stream. ChannelStreamIngest callers
    /// must verify against this binding (PermissionDenied on mismatch),
    /// so two subjects in one agent cannot reach each other's channel streams.
    pub subject: String,
    /// The agent the minting subject names. The harness link checks
    /// against this, because the harness knows agents and not subjects.
    pub agent: String,
    pub tx: mpsc::Sender<ChannelStreamOutbound>,
    /// Client-advertised tool methods this channel stream can render. Updated
    /// on every `ChannelStreamIngest` (last-sender-wins). The controller
    /// refuses to dispatch a `ServerRequest` whose method is not in
    /// this set.
    pub supported_methods: Mutex<HashSet<String>>,
    /// Abort handle for the per-stream heartbeat task. `None` until
    /// `spawn_heartbeat` runs for the channel stream.
    pub heartbeat_handle: Mutex<Option<AbortHandle>>,
}

/// Per-agent broadcast bus. The harness opens a `Subscribe`
/// stream per agent and drains the matching sender; `ChannelStreamIngest`
/// pushes inbound `UserMessage`s onto it. Capacity 16.
#[derive(Default)]
pub struct SubscriberRegistry {
    senders: RwLock<HashMap<String, broadcast::Sender<UserMessage>>>,
}

impl SubscriberRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to an agent's bus, creating the sender on first use.
    /// Returns a fresh receiver; the sender is retained so later
    /// `notify` calls and additional subscribers share one bus.
    pub async fn subscribe_or_create(&self, agent: &str) -> broadcast::Receiver<UserMessage> {
        {
            let senders = self.senders.read().await;
            if let Some(tx) = senders.get(agent) {
                return tx.subscribe();
            }
        }
        let mut senders = self.senders.write().await;
        senders
            .entry(agent.to_string())
            .or_insert_with(|| broadcast::channel(16).0)
            .subscribe()
    }

    /// Push a message onto an agent's bus. No-op (and no allocation)
    /// when no subscriber has ever opened the agent — a message with
    /// no harness listening is dropped.
    pub async fn notify(&self, agent: &str, message: UserMessage) {
        let senders = self.senders.read().await;
        if let Some(tx) = senders.get(agent) {
            let _ = tx.send(message);
        }
    }
}

pub struct GatewayState {
    /// channel_stream_id (UUID, server-minted) → ChannelStreamEntry.
    channel_streams: RwLock<HashMap<String, ChannelStreamEntry>>,
    /// Last recorded turn phase of one agent's conversation, whoever started
    /// the turn. Backs `GetTurnState` and the replay on a fresh
    /// ChannelStreamReceive, so a client that missed the pushed
    /// `TurnStateEvent` (reconnect, late open) gets the relay-owned truth.
    /// `reason`/`code` are populated only for `Failed`; empty otherwise.
    last_turn_state: RwLock<HashMap<String, TurnStateEvent>>,
    /// Per-agent inbound user-message bus.
    subscribers: SubscriberRegistry,
    /// The live identity table, swapped by the identity watcher on every
    /// relay-identity ConfigMap delivery. It answers the agent list and
    /// resolves channel handles to subjects; authorization itself is a
    /// per-request Kubernetes review.
    identity: Arc<RwLock<IdentityTable>>,
    /// call_id → the (subject, agent) that dispatched it. The harness
    /// mints the call_id, so a CancelToolRequest — which carries no agent —
    /// resolves its agent here.
    dispatched_calls: RwLock<HashMap<String, (String, String)>>,
    kube_client: Option<kube::Client>,
    namespace: String,
    /// Pool of per-agent harness clients every forwarding handler dials.
    harness_clients: Arc<HarnessClientPool>,
    idp_issuer: String,
    idp_audience: String,
    idp_certificate_authority: String,
    /// Keyed by (agent, request_id) so a `ClientResponse` from any of the
    /// agent's channel streams resolves the awaiter.
    pending_server_requests:
        RwLock<HashMap<(String, String), oneshot::Sender<ServerRequestOutcome>>>,
    /// Heartbeat interval. Each channel stream receives an empty `Heartbeat` frame
    /// at this interval. `Duration::ZERO` disables heartbeats.
    heartbeat_interval: Duration,
}

impl GatewayState {
    pub fn new(kube_client: Option<kube::Client>, namespace: String) -> Self {
        let harness_clients = HarnessClientPool::new(&namespace);
        Self {
            channel_streams: RwLock::new(HashMap::new()),
            last_turn_state: RwLock::new(HashMap::new()),
            subscribers: SubscriberRegistry::new(),
            identity: Arc::new(RwLock::new(IdentityTable::default())),
            dispatched_calls: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            harness_clients,
            idp_issuer: String::new(),
            idp_audience: String::new(),
            idp_certificate_authority: String::new(),
            pending_server_requests: RwLock::new(HashMap::new()),
            heartbeat_interval: Duration::from_secs(15),
        }
    }

    /// Set the heartbeat interval. `Duration::ZERO` disables heartbeats.
    pub fn set_heartbeat_interval(&mut self, interval: Duration) {
        self.heartbeat_interval = interval;
    }

    /// Construct a `GatewayState` with IdP configuration.
    pub fn new_with_idp(
        kube_client: Option<kube::Client>,
        namespace: String,
        idp_issuer: String,
        idp_audience: String,
        idp_certificate_authority: String,
    ) -> Self {
        Self {
            idp_issuer,
            idp_audience,
            idp_certificate_authority,
            ..Self::new(kube_client, namespace)
        }
    }

    /// Like [`GatewayState::new`], but the harness pool dials `template`
    /// (`{agent}` substituted) instead of the in-cluster Service DNS, so a
    /// test can stand a harness up on a loopback port.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn with_harness_template(
        kube_client: Option<kube::Client>,
        namespace: String,
        template: String,
    ) -> Self {
        Self {
            harness_clients: HarnessClientPool::from_service_template(template),
            ..Self::new(kube_client, namespace)
        }
    }

    /// Shared handle on the live identity table. The identity watcher writes
    /// through it; the agent-list answer and handle resolution read it.
    pub fn identity(&self) -> Arc<RwLock<IdentityTable>> {
        self.identity.clone()
    }

    /// Returns the IdP configuration (issuer, audience, certificate_authority).
    pub fn idp_config(&self) -> (&str, &str, &str) {
        (
            &self.idp_issuer,
            &self.idp_audience,
            &self.idp_certificate_authority,
        )
    }

    /// Record which (subject, agent) dispatched a call, so a later
    /// CancelToolRequest resolves its agent without a wire field.
    pub async fn record_dispatched_call(&self, call_id: &str, subject: &str, agent: &str) {
        self.dispatched_calls.write().await.insert(
            call_id.to_string(),
            (subject.to_string(), agent.to_string()),
        );
    }

    /// The (subject, agent) a call_id was dispatched under, if the relay
    /// has seen it since this process started.
    pub async fn dispatched_call(&self, call_id: &str) -> Option<(String, String)> {
        self.dispatched_calls.read().await.get(call_id).cloned()
    }

    pub fn kube_client(&self) -> Option<&kube::Client> {
        self.kube_client.as_ref()
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn harness_clients(&self) -> &Arc<HarnessClientPool> {
        &self.harness_clients
    }

    pub async fn subscribe_or_create(&self, agent: &str) -> broadcast::Receiver<UserMessage> {
        self.subscribers.subscribe_or_create(agent).await
    }

    pub async fn notify_subscriber(&self, agent: &str, message: UserMessage) {
        self.subscribers.notify(agent, message).await;
    }

    /// Mint a fresh channel_stream_id (UUID), bind it to the minting subject
    /// and that subject's agent, and store the tx for outbound routing.
    /// Returns the channel_stream_id; the caller is responsible for echoing it
    /// back to the client as the first frame of the outbound stream
    /// (ChannelStreamAck).
    pub async fn mint_channel_stream(
        &self,
        opener: ChannelStreamOpener,
        subject: String,
        agent: String,
        tx: mpsc::Sender<ChannelStreamOutbound>,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.channel_streams.write().await.insert(
            id.clone(),
            ChannelStreamEntry {
                opener,
                subject,
                agent,
                tx,
                supported_methods: Mutex::new(HashSet::new()),
                heartbeat_handle: Mutex::new(None),
            },
        );
        id
    }

    pub async fn unregister_channel_stream(&self, channel_stream_id: &str) {
        if let Some(entry) = self.channel_streams.write().await.remove(channel_stream_id) {
            // Cancel the per-stream heartbeat task if running.
            if let Some(handle) = entry.heartbeat_handle.lock().await.take() {
                handle.abort();
            }
        }
    }

    /// Fan out a frame to all channel streams in `agent`. Returns the number
    /// of channel streams the frame was delivered to (sends that succeeded).
    pub async fn send_to_agent(&self, agent: &str, msg: ChannelStreamOutbound) -> usize {
        let senders = self.agent_senders(agent).await;
        send_to_each(agent, senders, msg).await
    }

    /// Clone the agent's stream senders, so no lock is held while a send
    /// waits on a stream that isn't being read.
    async fn agent_senders(
        &self,
        agent: &str,
    ) -> Vec<(String, mpsc::Sender<ChannelStreamOutbound>)> {
        self.channel_streams
            .read()
            .await
            .iter()
            .filter(|(_, e)| e.agent == agent)
            .map(|(id, e)| (id.clone(), e.tx.clone()))
            .collect()
    }

    /// Spawn a per-stream heartbeat task. The task sends an empty
    /// `Heartbeat` frame every configured heartbeat interval. Stores the
    /// abort handle on the channel stream entry so `unregister_channel_stream` cancels
    /// it. No-op when the interval is zero.
    pub async fn spawn_heartbeat(&self, channel_stream_id: &str) {
        let interval = self.heartbeat_interval;
        if interval.is_zero() {
            return;
        }
        let channel_streams = self.channel_streams.read().await;
        let Some(entry) = channel_streams.get(channel_stream_id) else {
            return;
        };
        let tx = entry.tx.clone();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // First tick is immediate, skip it.
            loop {
                ticker.tick().await;
                let msg = ChannelStreamOutbound {
                    command: Some(channel_stream_outbound::Command::Heartbeat(Heartbeat {})),
                };
                if tx.send(msg).await.is_err() {
                    break;
                }
            }
        });
        let abort_handle = task.abort_handle();
        *entry.heartbeat_handle.lock().await = Some(abort_handle);
    }

    /// Return who opened a channel_stream_id and the `(subject, agent)` it is
    /// bound to, in one read, or None if unknown. ChannelStreamIngest admits only
    /// the caller that opened it. A caller that needs more than one field must
    /// not take two locks: the channel stream can be unregistered between them,
    /// leaving the second lookup empty while the first said the channel stream was
    /// live.
    pub async fn channel_stream_binding(
        &self,
        channel_stream_id: &str,
    ) -> Option<(ChannelStreamOpener, String, String)> {
        self.channel_streams
            .read()
            .await
            .get(channel_stream_id)
            .map(|entry| {
                (
                    entry.opener.clone(),
                    entry.subject.clone(),
                    entry.agent.clone(),
                )
            })
    }

    /// Record `event`'s state, reason and code as the agent's turn phase, and
    /// send the whole `event` to every stream of the agent. The streams are
    /// picked under the record's write lock, so a stream that replayed the
    /// old record still gets this frame after it; the sends run after the
    /// lock is released. The record is kept even when no stream is open, so a
    /// later poll or open recovers it. Returns the number of channel streams
    /// the frame was delivered to.
    pub async fn set_and_broadcast_turn_state(&self, agent: &str, event: TurnStateEvent) -> usize {
        let senders = {
            let mut records = self.last_turn_state.write().await;
            records.insert(
                agent.to_string(),
                TurnStateEvent {
                    state: event.state,
                    reason: event.reason.clone(),
                    code: event.code.clone(),
                    ..Default::default()
                },
            );
            self.agent_senders(agent).await
        };
        let msg = ChannelStreamOutbound {
            command: Some(channel_stream_outbound::Command::TurnState(event)),
        };
        send_to_each(agent, senders, msg).await
    }

    /// Read the agent's recorded turn phase, IDLE when none is recorded.
    pub async fn turn_state_record(&self, agent: &str) -> TurnStateEvent {
        recorded_or_idle(&*self.last_turn_state.read().await, agent)
    }

    /// Send the channel stream its agent's recorded turn phase (IDLE when
    /// none) without modifying it. Used by `ChannelStreamReceive` right after
    /// the initial `ChannelStreamAck`, so a stream opened mid-turn starts on
    /// WORKING like every stream already open. `try_send`, so no lock waits
    /// on a send; a stream that is already full drops the replay, and
    /// GetTurnState still answers.
    pub async fn replay_turn_state(&self, channel_stream_id: &str) {
        let records = self.last_turn_state.read().await;
        let channel_streams = self.channel_streams.read().await;
        let Some(entry) = channel_streams.get(channel_stream_id) else {
            return;
        };
        let event = recorded_or_idle(&records, &entry.agent);
        let msg = ChannelStreamOutbound {
            command: Some(channel_stream_outbound::Command::TurnState(event)),
        };
        let _ = entry.tx.try_send(msg);
    }

    /// Replace the channel stream's advertised supported_methods set. Called by
    /// `channel_stream_ingest` on every inbound request because the client may
    /// change devices (each device declares its own renderer set).
    /// Returns false if the channel stream is no longer registered.
    pub async fn update_supported_methods(
        &self,
        channel_stream_id: &str,
        methods: Vec<String>,
    ) -> bool {
        let channel_streams = self.channel_streams.read().await;
        let Some(entry) = channel_streams.get(channel_stream_id) else {
            return false;
        };
        let mut guard = entry.supported_methods.lock().await;
        *guard = methods.into_iter().collect();
        true
    }

    /// Deliver a client's `ClientResponse` to whichever
    /// `send_server_request_and_await` awaiter is parked on the matching
    /// request_id. Returns true if a matching pending request was found
    /// and the outcome delivered; false otherwise (unknown agent,
    /// unknown request_id, or awaiter already dropped — all benign).
    pub async fn deliver_client_response(
        &self,
        agent: &str,
        request_id: &str,
        outcome: ServerRequestOutcome,
    ) -> bool {
        let key = (agent.to_string(), request_id.to_string());
        let mut pending = self.pending_server_requests.write().await;
        let Some(sender) = pending.remove(&key) else {
            return false;
        };
        sender.send(outcome).is_ok()
    }

    /// Fire-and-forget notification to all channel streams in `agent` that
    /// support `method`. Empty `request_id` on the wire signals to the
    /// client that no response is expected.
    pub async fn send_server_notification(
        &self,
        agent: &str,
        method: &str,
        params_json: String,
    ) -> Result<(), ServerRequestError> {
        let channel_streams = self.channel_streams.read().await;
        let agent_channel_streams: Vec<_> = channel_streams
            .values()
            .filter(|e| e.agent == agent)
            .collect();
        if agent_channel_streams.is_empty() {
            return Err(ServerRequestError::UnknownChannelStream);
        }
        let mut any_sent = false;
        for entry in agent_channel_streams {
            if !entry.supported_methods.lock().await.contains(method) {
                continue;
            }
            let msg = ChannelStreamOutbound {
                command: Some(channel_stream_outbound::Command::ServerRequest(
                    ServerRequest {
                        request_id: String::new(),
                        method: method.to_string(),
                        params_json: params_json.clone(),
                    },
                )),
            };
            if entry.tx.send(msg).await.is_ok() {
                any_sent = true;
            }
        }
        if !any_sent {
            return Err(ServerRequestError::UnsupportedMethod);
        }
        Ok(())
    }

    /// Send a server-initiated request to every channel stream in `agent`,
    /// once at least one supports `method`, and await the matching
    /// `ClientResponse`. The pending-request registry is agent-wide
    /// so a response from any channel stream in the agent resolves it.
    ///
    /// Timeout cleanup is best-effort: on `Err(Timeout)` we remove the
    /// pending slot so a late `ClientResponse` doesn't leak memory. A
    /// late response arriving for a removed slot is dropped silently
    /// (deliver_client_response returns false).
    pub async fn send_server_request_and_await(
        self: &Arc<Self>,
        agent: &str,
        request_id: &str,
        method: &str,
        params_json: String,
        timeout: Duration,
    ) -> Result<ServerRequestOutcome, ServerRequestError> {
        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        {
            let channel_streams = self.channel_streams.read().await;
            let agent_channel_streams: Vec<_> = channel_streams
                .values()
                .filter(|e| e.agent == agent)
                .collect();
            if agent_channel_streams.is_empty() {
                return Err(ServerRequestError::UnknownChannelStream);
            }
            let any_supports = {
                let mut found = false;
                for entry in &agent_channel_streams {
                    if entry.supported_methods.lock().await.contains(method) {
                        found = true;
                        break;
                    }
                }
                found
            };
            if !any_supports {
                return Err(ServerRequestError::UnsupportedMethod);
            }
            self.pending_server_requests
                .write()
                .await
                .insert((agent.to_string(), request_id.to_string()), tx_oneshot);
            let msg = ChannelStreamOutbound {
                command: Some(channel_stream_outbound::Command::ServerRequest(
                    ServerRequest {
                        request_id: request_id.to_string(),
                        method: method.to_string(),
                        params_json,
                    },
                )),
            };
            let mut any_sent = false;
            for entry in &agent_channel_streams {
                if entry.tx.send(msg.clone()).await.is_ok() {
                    any_sent = true;
                }
            }
            if !any_sent {
                self.pending_server_requests
                    .write()
                    .await
                    .remove(&(agent.to_string(), request_id.to_string()));
                return Err(ServerRequestError::SendFailed);
            }
        }
        let agent_owned = agent.to_string();
        let request_id_owned = request_id.to_string();
        match tokio::time::timeout(timeout, rx_oneshot).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Err(ServerRequestError::Disconnected),
            Err(_) => {
                self.pending_server_requests
                    .write()
                    .await
                    .remove(&(agent_owned, request_id_owned));
                Err(ServerRequestError::Timeout)
            }
        }
    }
}

fn recorded_or_idle(records: &HashMap<String, TurnStateEvent>, agent: &str) -> TurnStateEvent {
    records.get(agent).cloned().unwrap_or(TurnStateEvent {
        state: TurnState::Idle as i32,
        ..Default::default()
    })
}

/// Send `msg` to each stream; returns how many sends succeeded.
async fn send_to_each(
    agent: &str,
    senders: Vec<(String, mpsc::Sender<ChannelStreamOutbound>)>,
    msg: ChannelStreamOutbound,
) -> usize {
    let mut delivered = 0;
    for (channel_stream_id, tx) in senders {
        tracing::info!(agent = %agent, channel_stream_id = %channel_stream_id, "send_to_each: channel stream");
        if tx.send(msg.clone()).await.is_ok() {
            delivered += 1;
        }
    }
    delivered
}

/// Fixtures shared by the crate's unit tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn make_state() -> Arc<GatewayState> {
        Arc::new(GatewayState::new(None, "default".into()))
    }

    /// Mint a person-opened channel stream for `subject` on `agent`.
    pub(crate) async fn mint_person(
        state: &GatewayState,
        subject: &str,
        agent: &str,
        tx: mpsc::Sender<ChannelStreamOutbound>,
    ) -> String {
        state
            .mint_channel_stream(
                ChannelStreamOpener::Person,
                subject.into(),
                agent.into(),
                tx,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{make_state, mint_person};
    use super::*;

    fn extract_turn_state_event(msg: &ChannelStreamOutbound) -> Option<&TurnStateEvent> {
        match &msg.command {
            Some(channel_stream_outbound::Command::TurnState(e)) => Some(e),
            _ => None,
        }
    }

    #[tokio::test]
    async fn mint_channel_stream_returns_unique_ids() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(1);
        let a = mint_person(&state, "subject-ag", "ag", tx.clone()).await;
        let b = mint_person(&state, "subject-ag", "ag", tx).await;
        assert_ne!(a, b, "each mint must produce a distinct channel_stream_id");
    }

    #[tokio::test]
    async fn unregister_channel_stream_removes() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(1);
        let id = mint_person(&state, "subject-ag", "ag", tx).await;
        state.unregister_channel_stream(&id).await;
        assert!(state.channel_stream_binding(&id).await.is_none());
    }

    #[tokio::test]
    async fn set_and_broadcast_turn_state_emits_and_records_the_phase() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(4);
        mint_person(&state, "subject-ag", "ag", tx).await;
        let delivered = state
            .set_and_broadcast_turn_state(
                "ag",
                TurnStateEvent {
                    state: TurnState::Working as i32,
                    ..Default::default()
                },
            )
            .await;
        assert_eq!(delivered, 1);
        let msg = rx.recv().await.unwrap();
        let event = extract_turn_state_event(&msg).unwrap();
        assert_eq!(event.state, TurnState::Working as i32);
        let rec = state.turn_state_record("ag").await;
        assert_eq!(rec.state(), TurnState::Working);
    }

    #[tokio::test]
    async fn set_and_broadcast_with_no_open_channel_stream_records_but_reports_unsent() {
        let state = make_state();
        assert_eq!(
            state
                .set_and_broadcast_turn_state(
                    "ag",
                    TurnStateEvent {
                        state: TurnState::Working as i32,
                        ..Default::default()
                    },
                )
                .await,
            0,
            "an agent with no open stream reports nothing sent"
        );
        let rec = state.turn_state_record("ag").await;
        assert_eq!(rec.state(), TurnState::Working);
    }

    #[tokio::test]
    async fn notify_subscriber_does_not_leak_to_other_agent() {
        let state = make_state();
        let mut alpha_rx = state.subscribe_or_create("alpha").await;
        let mut beta_rx = state.subscribe_or_create("beta").await;
        state
            .notify_subscriber(
                "alpha",
                UserMessage {
                    content: vec![],
                    sender: "only-alpha".into(),
                    reply_channel_stream: None,
                    message_id: String::new(),
                },
            )
            .await;
        assert_eq!(alpha_rx.recv().await.unwrap().sender, "only-alpha");
        assert!(
            beta_rx.try_recv().is_err(),
            "beta's bus must not receive alpha's message"
        );
    }

    #[tokio::test]
    async fn notify_unknown_agent_is_noop() {
        // No subscriber has opened the agent → message dropped, no panic.
        let state = make_state();
        state
            .notify_subscriber(
                "never-opened",
                UserMessage {
                    content: vec![],
                    sender: "x".into(),
                    reply_channel_stream: None,
                    message_id: String::new(),
                },
            )
            .await;
    }

    #[tokio::test]
    async fn send_server_notification_unsupported_method_errors() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(4);
        let _id = mint_person(&state, "subject-ag", "ag", tx).await;
        // No supported_methods advertised → unsupported.
        let err = state
            .send_server_notification("ag", "RevealPath", "{}".into())
            .await
            .unwrap_err();
        assert!(matches!(err, ServerRequestError::UnsupportedMethod));
    }

    #[tokio::test]
    async fn update_supported_methods_unknown_channel_stream_is_false() {
        let state = make_state();
        assert!(
            !state
                .update_supported_methods("ghost", vec!["X".into()])
                .await
        );
    }

    #[tokio::test]
    async fn send_server_notification_fans_out_to_every_supporting_channel_stream() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(4);
        let (tx_b, mut rx_b) = mpsc::channel(4);
        let id = mint_person(&state, "subject-ag", "ag", tx).await;
        let id_b = mint_person(&state, "subject-ag", "ag", tx_b).await;
        state
            .update_supported_methods(&id, vec!["RevealPath".into()])
            .await;
        state
            .update_supported_methods(&id_b, vec!["RevealPath".into()])
            .await;
        state
            .send_server_notification("ag", "RevealPath", "{\"p\":1}".into())
            .await
            .unwrap();
        assert!(
            rx_b.recv().await.is_some(),
            "every supporting agent channel stream must receive"
        );
        let msg = rx.recv().await.unwrap();
        match msg.command {
            Some(channel_stream_outbound::Command::ServerRequest(sr)) => {
                assert_eq!(sr.method, "RevealPath");
                assert!(
                    sr.request_id.is_empty(),
                    "notification carries empty request_id"
                );
            }
            other => panic!("expected ServerRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_server_request_and_await_rejects_unsupported_method_without_dispatch() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(4);
        let _id = mint_person(&state, "subject-ag", "ag", tx).await;
        // No supported_methods advertised → reject before dispatch.
        let result = state
            .send_server_request_and_await(
                "ag",
                "req-1",
                "AskUser",
                "{}".into(),
                Duration::from_millis(200),
            )
            .await;
        assert!(matches!(result, Err(ServerRequestError::UnsupportedMethod)));
        assert!(
            rx.try_recv().is_err(),
            "no frame should be dispatched for an unsupported method"
        );
    }

    #[tokio::test]
    async fn send_server_request_and_await_times_out_and_cleans_up() {
        tokio::time::pause();
        let state = make_state();
        let (tx, _rx) = mpsc::channel(4);
        let id = mint_person(&state, "subject-ag", "ag", tx).await;
        state
            .update_supported_methods(&id, vec!["AskUser".into()])
            .await;
        let handle = tokio::spawn(async move {
            state
                .send_server_request_and_await(
                    "ag",
                    "req-1",
                    "AskUser",
                    "{}".into(),
                    Duration::from_secs(5),
                )
                .await
        });
        tokio::time::advance(Duration::from_secs(6)).await;
        let result = handle.await.unwrap();
        assert!(matches!(result, Err(ServerRequestError::Timeout)));
    }

    #[tokio::test]
    async fn send_server_request_and_await_delivers_client_response() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(4);
        let id = mint_person(&state, "subject-ag", "ag", tx).await;
        state
            .update_supported_methods(&id, vec!["AskUser".into()])
            .await;
        let state_for_await = state.clone();
        let handle = tokio::spawn(async move {
            state_for_await
                .send_server_request_and_await(
                    "ag",
                    "req-7",
                    "AskUser",
                    "{}".into(),
                    Duration::from_secs(5),
                )
                .await
        });
        // Drain the dispatched ServerRequest frame, then deliver a response.
        let _frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("ServerRequest frame must be dispatched before await")
            .unwrap();
        assert!(
            !state
                .deliver_client_response("", "req-7", ServerRequestOutcome::Result("ok".into()))
                .await,
            "empty agent should not match"
        );
        assert!(
            state
                .deliver_client_response("ag", "req-7", ServerRequestOutcome::Result("ok".into()))
                .await
        );
        let outcome = handle.await.unwrap().unwrap();
        match outcome {
            ServerRequestOutcome::Result(s) => assert_eq!(s, "ok"),
            ServerRequestOutcome::Error(_) => panic!("expected Result outcome"),
        }
    }

    #[tokio::test]
    async fn send_to_agent_fans_out_to_all_live_channel_streams() {
        let state = make_state();
        let (tx_a, mut rx_a) = mpsc::channel(4);
        let (tx_b, mut rx_b) = mpsc::channel(4);
        let _a = mint_person(&state, "subject-ag", "ag", tx_a).await;
        let _b = mint_person(&state, "subject-ag", "ag", tx_b).await;
        let msg = ChannelStreamOutbound {
            command: Some(channel_stream_outbound::Command::Ack(
                proto_common::ChannelStreamAck {
                    channel_stream_id: "test".into(),
                },
            )),
        };
        let delivered = state.send_to_agent("ag", msg).await;
        assert_eq!(delivered, 2);
        assert!(rx_a.recv().await.is_some());
        assert!(rx_b.recv().await.is_some());
    }
}
