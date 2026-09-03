//! In-memory state owned by the internet-facing gateway pod.
//!
//! The gateway does NOT own a conversation registry or event log — those
//! live in the per-workspace harness, reached via the
//! `HarnessClientPool`. The gateway owns the *live* surface:
//!
//! - `channels` — server-minted channel_id → outbound mpsc + per-channel
//!   turn phase + pending server-requests + advertised methods.
//! - `last_turn_state` — per-conversation last recorded phase, backing the
//!   `GetTurnState` poll.
//! - `SubscriberRegistry` — per-workspace broadcast bus the harness's
//!   `Subscribe` stream drains; `ChannelIngest` notifies it. Standalone
//!   because the gateway has no per-workspace conversation state to
//!   attach it to.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use proto_common::{
    channel_outbound, ChannelOutbound, ClientResponseError, ServerRequest, TurnState,
    TurnStateEvent, UserMessage,
};
use shared::client_signature::ClientSignatureVerifier;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};

use crate::grants::RelayGrants;
use crate::harness_client::HarnessClientPool;

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
/// `mint_channel`; lifetime tied to the originating ChannelReceive
/// response stream.
pub struct ChannelEntry {
    /// The grant row that minted this channel. ChannelIngest callers
    /// must verify against this binding (PermissionDenied on mismatch),
    /// so two rows in one workspace cannot reach each other's channels.
    pub row: String,
    /// The workspace the minting row names. The harness link checks
    /// against this, because the harness knows workspaces and not rows.
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

/// Last recorded turn phase for one conversation. Backs `GetTurnState`
/// so a client that missed the pushed `TurnStateEvent` (reconnect, dropped
/// receive stream) can poll the controller-owned truth. `reason`/`code`
/// are populated only for `Failed`; empty otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnStateRecord {
    pub state: TurnState,
    pub reason: String,
    pub code: String,
}

/// Per-workspace broadcast bus. The harness opens a `Subscribe`
/// stream per workspace and drains the matching sender; `ChannelIngest`
/// pushes inbound `UserMessage`s onto it. Capacity 16.
#[derive(Default)]
pub struct SubscriberRegistry {
    senders: RwLock<HashMap<String, broadcast::Sender<UserMessage>>>,
}

impl SubscriberRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to a workspace's bus, creating the sender on first use.
    /// Returns a fresh receiver; the sender is retained so later
    /// `notify` calls and additional subscribers share one bus.
    pub async fn subscribe_or_create(&self, workspace: &str) -> broadcast::Receiver<UserMessage> {
        {
            let senders = self.senders.read().await;
            if let Some(tx) = senders.get(workspace) {
                return tx.subscribe();
            }
        }
        let mut senders = self.senders.write().await;
        senders
            .entry(workspace.to_string())
            .or_insert_with(|| broadcast::channel(16).0)
            .subscribe()
    }

    /// Push a message onto a workspace's bus. No-op (and no allocation)
    /// when no subscriber has ever opened the workspace — a message with
    /// no harness listening is dropped.
    pub async fn notify(&self, workspace: &str, message: UserMessage) {
        let senders = self.senders.read().await;
        if let Some(tx) = senders.get(workspace) {
            let _ = tx.send(message);
        }
    }
}

pub struct GatewayState {
    /// channel_id (UUID, server-minted) → ChannelEntry.
    channels: RwLock<HashMap<String, ChannelEntry>>,
    /// (grant row, conversation_id) → last recorded turn phase. Keyed by
    /// the caller's verified row so a `GetTurnState` poll can only read
    /// its own row's phase; conversation_id alone is a bare UUID and would
    /// leak across a tenant's rows. Written on every turn-state broadcast;
    /// read by the poll.
    last_turn_state: RwLock<HashMap<(String, String), TurnStateRecord>>,
    /// Per-workspace inbound user-message bus.
    subscribers: SubscriberRegistry,
    /// Registered device keys, one per grant row. Rebuilt from the
    /// relay-owned Secret at startup, written by each redemption, read by
    /// the signature middleware on every signed request.
    client_verifier: Arc<ClientSignatureVerifier>,
    /// The live authorization table, swapped by the grants watcher on every
    /// ConfigMap delivery. Every request is checked against it, so removing
    /// a row cuts access within seconds and without a pod restart.
    grants: Arc<RwLock<RelayGrants>>,
    /// conversation_id → the grant row that minted it. A cache, not the
    /// record: the harness holds the durable stamp. A miss resolves against
    /// the harness; it never reads as "unowned, therefore fine".
    conversation_owners: RwLock<HashMap<String, String>>,
    kube_client: Option<kube::Client>,
    namespace: String,
    /// Pool of per-workspace harness clients for the tool forwards
    /// (`WatchTools`/`CallTool`) and the conversation-lifecycle forwards.
    harness_clients: Arc<HarnessClientPool>,
    /// Per-workspace credential-grant menu, read from the mounted bindings
    /// file at startup. Names only; empty when no bindings file is mounted.
    credentials: crate::credentials::CredentialMenu,
}

impl GatewayState {
    pub fn new(
        client_verifier: Arc<ClientSignatureVerifier>,
        kube_client: Option<kube::Client>,
        namespace: String,
    ) -> Self {
        let harness_clients = HarnessClientPool::new(&namespace);
        Self {
            channels: RwLock::new(HashMap::new()),
            last_turn_state: RwLock::new(HashMap::new()),
            subscribers: SubscriberRegistry::new(),
            client_verifier,
            grants: Arc::new(RwLock::new(RelayGrants::default())),
            conversation_owners: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            harness_clients,
            credentials: crate::credentials::CredentialMenu::default(),
        }
    }

    /// Install the startup-loaded credential menu.
    pub fn with_credentials(mut self, credentials: crate::credentials::CredentialMenu) -> Self {
        self.credentials = credentials;
        self
    }

    pub fn credentials(&self) -> &crate::credentials::CredentialMenu {
        &self.credentials
    }

    #[cfg(test)]
    pub fn new_with_harness_pool(
        client_verifier: Arc<ClientSignatureVerifier>,
        kube_client: Option<kube::Client>,
        namespace: String,
        harness_clients: Arc<HarnessClientPool>,
    ) -> Self {
        Self {
            channels: RwLock::new(HashMap::new()),
            last_turn_state: RwLock::new(HashMap::new()),
            subscribers: SubscriberRegistry::new(),
            client_verifier,
            grants: Arc::new(RwLock::new(RelayGrants::default())),
            conversation_owners: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            harness_clients,
            credentials: crate::credentials::CredentialMenu::default(),
        }
    }

    pub fn client_verifier(&self) -> &Arc<ClientSignatureVerifier> {
        &self.client_verifier
    }

    /// Shared handle on the live authorization table. The grants watcher
    /// writes through it; every request reads through it.
    pub fn grants(&self) -> Arc<RwLock<RelayGrants>> {
        self.grants.clone()
    }

    /// The grant row a conversation is cached under, if the relay has seen
    /// it since this process started.
    pub async fn conversation_owner(&self, conversation_id: &str) -> Option<String> {
        self.conversation_owners
            .read()
            .await
            .get(conversation_id)
            .cloned()
    }

    /// Cache a conversation's owning row after minting it or resolving it
    /// against the harness.
    pub async fn record_conversation_owner(&self, conversation_id: &str, row: &str) {
        self.conversation_owners
            .write()
            .await
            .insert(conversation_id.to_string(), row.to_string());
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

    pub async fn subscribe_or_create(&self, workspace: &str) -> broadcast::Receiver<UserMessage> {
        self.subscribers.subscribe_or_create(workspace).await
    }

    pub async fn notify_subscriber(&self, workspace: &str, message: UserMessage) {
        self.subscribers.notify(workspace, message).await;
    }

    /// Mint a fresh channel_id (UUID), bind it to the minting grant row
    /// and that row's workspace, and store the tx for outbound routing.
    /// Returns the channel_id; the caller is responsible for echoing it
    /// back to the adapter as the first frame of the outbound stream
    /// (ChannelAck).
    pub async fn mint_channel(
        &self,
        row: String,
        workspace: String,
        adapter_hint: Option<String>,
        tx: mpsc::Sender<ChannelOutbound>,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.channels.write().await.insert(
            id.clone(),
            ChannelEntry {
                row,
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

    /// Return the grant row bound to a channel_id, or None if unknown.
    /// ChannelIngest uses this: the caller's verified row MUST equal the
    /// channel's bound row, otherwise the call is rejected.
    pub async fn channel_row(&self, channel_id: &str) -> Option<String> {
        self.channels
            .read()
            .await
            .get(channel_id)
            .map(|entry| entry.row.clone())
    }

    /// Return the workspace bound to a channel_id, or None if unknown.
    /// The harness link checks against this, because the harness knows
    /// workspaces and not rows.
    pub async fn channel_workspace(&self, channel_id: &str) -> Option<String> {
        self.channels
            .read()
            .await
            .get(channel_id)
            .map(|entry| entry.workspace.clone())
    }

    /// Return the `(row, workspace)` a channel_id is bound to, in one read.
    /// A caller that needs both must not take two locks: the channel can be
    /// unregistered between them, leaving the second lookup empty while the
    /// first said the channel was live.
    pub async fn channel_binding(&self, channel_id: &str) -> Option<(String, String)> {
        self.channels
            .read()
            .await
            .get(channel_id)
            .map(|entry| (entry.row.clone(), entry.workspace.clone()))
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
        row: &str,
        conversation_id: &str,
        state: TurnState,
    ) -> bool {
        // Record before the channel lookup so the per-conversation phase is
        // captured even when the channel has gone away (client disconnected
        // mid-turn) — the client recovers it via GetTurnState on reconnect.
        self.record_turn_state(
            row,
            conversation_id,
            TurnStateRecord {
                state,
                reason: String::new(),
                code: String::new(),
            },
        )
        .await;
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
                ..Default::default()
            })),
        };
        entry.tx.send(msg).await.is_ok()
    }

    /// Like `set_and_broadcast_turn_state`, but emits `FAILED` carrying a
    /// human-readable `reason` and a machine-readable `code`, and records
    /// the same so a later `GetTurnState` poll recovers the failure.
    /// Records before the channel lookup (a torn-down channel still needs
    /// the recorded FAILED for the reconnect poll). Returns false if the
    /// channel is no longer registered.
    pub async fn set_and_broadcast_turn_failed(
        &self,
        channel_id: &str,
        row: &str,
        conversation_id: &str,
        reason: &str,
        code: &str,
    ) -> bool {
        self.record_turn_state(
            row,
            conversation_id,
            TurnStateRecord {
                state: TurnState::Failed,
                reason: reason.to_string(),
                code: code.to_string(),
            },
        )
        .await;
        let channels = self.channels.read().await;
        let Some(entry) = channels.get(channel_id) else {
            return false;
        };
        let mut guard = entry.current_state.lock().await;
        *guard = TurnState::Failed;
        let msg = ChannelOutbound {
            command: Some(channel_outbound::Command::TurnState(TurnStateEvent {
                state: TurnState::Failed as i32,
                conversation_id: conversation_id.to_string(),
                reason: reason.to_string(),
                code: code.to_string(),
                ..Default::default()
            })),
        };
        entry.tx.send(msg).await.is_ok()
    }

    /// Record the latest turn phase for a conversation so a later
    /// `GetTurnState` poll can recover it. Most-recent transition wins
    /// (unconditional overwrite). Empty `conversation_id` is ignored —
    /// channel-wide replay frames carry no conversation context and must
    /// not pollute the per-conversation map.
    pub async fn record_turn_state(
        &self,
        row: &str,
        conversation_id: &str,
        record: TurnStateRecord,
    ) {
        if conversation_id.is_empty() {
            return;
        }
        self.last_turn_state
            .write()
            .await
            .insert((row.to_string(), conversation_id.to_string()), record);
    }

    /// Read the recorded turn phase for a conversation. `None` when no
    /// transition has been recorded (fresh conversation, or the map was
    /// lost to a controller restart) — the `GetTurnState` handler maps
    /// that to IDLE, since absence of a record is not a failure.
    pub async fn turn_state_record(
        &self,
        row: &str,
        conversation_id: &str,
    ) -> Option<TurnStateRecord> {
        self.last_turn_state
            .read()
            .await
            .get(&(row.to_string(), conversation_id.to_string()))
            .cloned()
    }

    /// Replay the channel's current turn phase as a `TurnStateEvent`
    /// without modifying it. Used by `ChannelReceive` after the initial
    /// `ChannelAck` so fresh streams (reconnects, second-device opens)
    /// land in the correct visual state immediately. Emits an empty
    /// `conversation_id` — the channel's current_state is a per-channel
    /// summary, not per-conversation.
    pub async fn replay_turn_state(&self, channel_id: &str) -> bool {
        let channels = self.channels.read().await;
        let Some(entry) = channels.get(channel_id) else {
            return false;
        };
        let guard = entry.current_state.lock().await;
        let msg = ChannelOutbound {
            command: Some(channel_outbound::Command::TurnState(TurnStateEvent {
                state: *guard as i32,
                ..Default::default()
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

    /// Deliver a client's `ClientResponse` to whichever
    /// `send_server_request_and_await` awaiter is parked on the matching
    /// request_id. Returns true if a matching pending request was found
    /// and the outcome delivered; false otherwise (unknown channel,
    /// unknown request_id, or awaiter already dropped — all benign).
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
    /// the wire signals to the client that no response is expected.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_verifier() -> Arc<ClientSignatureVerifier> {
        Arc::new(ClientSignatureVerifier::new(Duration::from_secs(300)))
    }

    fn make_state() -> Arc<GatewayState> {
        Arc::new(GatewayState::new(
            fixture_verifier(),
            None,
            "default".into(),
        ))
    }

    fn extract_turn_state(msg: &ChannelOutbound) -> Option<TurnState> {
        match &msg.command {
            Some(channel_outbound::Command::TurnState(e)) => TurnState::try_from(e.state).ok(),
            _ => None,
        }
    }

    fn extract_turn_state_event(msg: &ChannelOutbound) -> Option<&TurnStateEvent> {
        match &msg.command {
            Some(channel_outbound::Command::TurnState(e)) => Some(e),
            _ => None,
        }
    }

    #[tokio::test]
    async fn mint_channel_and_send() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(4);
        let id = state
            .mint_channel("row-alpha".into(), "alpha".into(), None, tx)
            .await;
        assert!(
            state
                .send_to_channel(
                    &id,
                    ChannelOutbound {
                        command: Some(channel_outbound::Command::Ack(proto_common::ChannelAck {
                            channel_id: id.clone(),
                        })),
                    }
                )
                .await
        );
        let got = rx.recv().await.unwrap();
        assert!(matches!(
            got.command,
            Some(channel_outbound::Command::Ack(_))
        ));
    }

    #[tokio::test]
    async fn mint_channel_returns_unique_ids() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(1);
        let a = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx.clone())
            .await;
        let b = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        assert_ne!(a, b, "each mint must produce a distinct channel_id");
    }

    #[tokio::test]
    async fn channel_workspace_returns_binding() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(1);
        let id = state
            .mint_channel("row-hello".into(), "hello-world".into(), None, tx)
            .await;
        assert_eq!(
            state.channel_workspace(&id).await.as_deref(),
            Some("hello-world")
        );
        assert_eq!(state.channel_workspace("nonexistent").await, None);
    }

    #[tokio::test]
    async fn send_to_channel_does_not_leak_to_other_channel() {
        let state = make_state();
        let (tx_a, mut rx_a) = mpsc::channel(4);
        let (tx_b, mut rx_b) = mpsc::channel(4);
        let a = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx_a)
            .await;
        let _b = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx_b)
            .await;
        state
            .send_to_channel(
                &a,
                ChannelOutbound {
                    command: Some(channel_outbound::Command::Ack(proto_common::ChannelAck {
                        channel_id: a.clone(),
                    })),
                },
            )
            .await;
        assert!(rx_a.recv().await.is_some());
        // b must not have received anything.
        assert!(rx_b.try_recv().is_err());
    }

    #[tokio::test]
    async fn unregister_channel_removes() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(1);
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        state.unregister_channel(&id).await;
        assert_eq!(state.channel_workspace(&id).await, None);
    }

    #[tokio::test]
    async fn mint_channel_defaults_current_state_to_idle() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(4);
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        assert!(state.replay_turn_state(&id).await);
        let msg = rx.recv().await.unwrap();
        assert_eq!(extract_turn_state(&msg), Some(TurnState::Idle));
    }

    #[tokio::test]
    async fn set_and_broadcast_turn_state_updates_state_and_emits_frame() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(4);
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        assert!(
            state
                .set_and_broadcast_turn_state(&id, "ws", "ws.conv-1", TurnState::Working)
                .await
        );
        let msg = rx.recv().await.unwrap();
        assert_eq!(extract_turn_state(&msg), Some(TurnState::Working));
    }

    #[tokio::test]
    async fn set_and_broadcast_turn_state_emits_conversation_id() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(4);
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        assert!(
            state
                .set_and_broadcast_turn_state(&id, "ws", "ws.conv-1", TurnState::Working)
                .await
        );
        let msg = rx.recv().await.unwrap();
        let event = extract_turn_state_event(&msg).unwrap();
        assert_eq!(event.conversation_id, "ws.conv-1");
        assert_eq!(event.state, TurnState::Working as i32);
    }

    #[tokio::test]
    async fn set_and_broadcast_turn_state_returns_false_for_unknown_channel() {
        let state = make_state();
        assert!(
            !state
                .set_and_broadcast_turn_state("ghost", "ws", "ws.conv", TurnState::Working)
                .await
        );
    }

    #[tokio::test]
    async fn turn_state_transitions_arrive_in_fifo_order() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(8);
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        state
            .set_and_broadcast_turn_state(&id, "ws", "ws.c", TurnState::Working)
            .await;
        state
            .set_and_broadcast_turn_state(&id, "ws", "ws.c", TurnState::Idle)
            .await;
        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        assert_eq!(extract_turn_state(&first), Some(TurnState::Working));
        assert_eq!(extract_turn_state(&second), Some(TurnState::Idle));
    }

    #[tokio::test]
    async fn set_and_broadcast_turn_state_records_phase_for_poll() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(4);
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        state
            .set_and_broadcast_turn_state(&id, "ws", "ws.conv-x", TurnState::Working)
            .await;
        let rec = state.turn_state_record("ws", "ws.conv-x").await.unwrap();
        assert_eq!(rec.state, TurnState::Working);
    }

    #[tokio::test]
    async fn set_and_broadcast_records_even_when_channel_gone() {
        // The per-conversation record must survive a disconnected channel
        // so the reconnect poll recovers the phase.
        let state = make_state();
        state
            .set_and_broadcast_turn_state("ghost", "ws", "ws.conv-y", TurnState::Working)
            .await;
        let rec = state.turn_state_record("ws", "ws.conv-y").await.unwrap();
        assert_eq!(rec.state, TurnState::Working);
    }

    #[tokio::test]
    async fn set_and_broadcast_turn_failed_emits_and_records_reason_code() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(4);
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        assert!(
            state
                .set_and_broadcast_turn_failed(&id, "ws", "ws.conv-z", "prompt job reaped", "14")
                .await
        );
        let msg = rx.recv().await.unwrap();
        let event = extract_turn_state_event(&msg).unwrap();
        assert_eq!(event.state, TurnState::Failed as i32);
        // Kills state.rs conversation_id in the failed broadcast event:
        // dropping the field defaults it to empty, so a client watching the
        // stream could not route the FAILED frame to the right conversation.
        assert_eq!(event.conversation_id, "ws.conv-z");
        assert_eq!(event.reason, "prompt job reaped");
        assert_eq!(event.code, "14");
        let rec = state.turn_state_record("ws", "ws.conv-z").await.unwrap();
        assert_eq!(rec.state, TurnState::Failed);
        assert_eq!(rec.reason, "prompt job reaped");
        assert_eq!(rec.code, "14");
    }

    #[tokio::test]
    async fn turn_state_record_absent_for_unknown_conversation() {
        let state = make_state();
        assert!(state.turn_state_record("ws", "ws.never").await.is_none());
    }

    #[tokio::test]
    async fn turn_state_record_scoped_to_workspace() {
        let state = make_state();
        state
            .record_turn_state(
                "alpha",
                "conv",
                TurnStateRecord {
                    state: TurnState::Working,
                    reason: String::new(),
                    code: String::new(),
                },
            )
            .await;
        assert!(state.turn_state_record("alpha", "conv").await.is_some());
        // A sibling workspace with the same conversation_id sees nothing.
        assert!(state.turn_state_record("beta", "conv").await.is_none());
    }

    #[tokio::test]
    async fn record_turn_state_ignores_empty_conversation_id() {
        let state = make_state();
        state
            .record_turn_state(
                "ws",
                "",
                TurnStateRecord {
                    state: TurnState::Working,
                    reason: String::new(),
                    code: String::new(),
                },
            )
            .await;
        assert!(state.turn_state_record("ws", "").await.is_none());
    }

    #[tokio::test]
    async fn notify_subscriber_routes_to_correct_workspace() {
        let state = make_state();
        let mut rx = state.subscribe_or_create("alpha").await;
        state
            .notify_subscriber(
                "alpha",
                UserMessage {
                    content: vec![],
                    sender: "u".into(),
                    reply_channel: Some("chan-1".into()),
                    conversation_id: "alpha.c".into(),
                    grants: vec![],
                },
            )
            .await;
        let got = rx.recv().await.unwrap();
        assert_eq!(got.sender, "u");
        assert_eq!(got.reply_channel.as_deref(), Some("chan-1"));
    }

    #[tokio::test]
    async fn notify_subscriber_does_not_leak_to_other_workspace() {
        let state = make_state();
        let mut alpha_rx = state.subscribe_or_create("alpha").await;
        let mut beta_rx = state.subscribe_or_create("beta").await;
        state
            .notify_subscriber(
                "alpha",
                UserMessage {
                    content: vec![],
                    sender: "only-alpha".into(),
                    reply_channel: None,
                    conversation_id: "alpha.c".into(),
                    grants: vec![],
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
    async fn notify_unknown_workspace_is_noop() {
        // No subscriber has opened the workspace → message dropped, no panic.
        let state = make_state();
        state
            .notify_subscriber(
                "never-opened",
                UserMessage {
                    content: vec![],
                    sender: "x".into(),
                    reply_channel: None,
                    conversation_id: String::new(),
                    grants: vec![],
                },
            )
            .await;
    }

    #[tokio::test]
    async fn deliver_client_response_unknown_channel_is_false() {
        let state = make_state();
        assert!(
            !state
                .deliver_client_response(
                    "ghost",
                    "req-1",
                    ServerRequestOutcome::Result("{}".into())
                )
                .await
        );
    }

    #[tokio::test]
    async fn send_server_notification_unknown_channel_errors() {
        let state = make_state();
        let err = state
            .send_server_notification("ghost", "RevealPath", "{}".into())
            .await
            .unwrap_err();
        assert!(matches!(err, ServerRequestError::UnknownChannel));
    }

    #[tokio::test]
    async fn send_server_notification_unsupported_method_errors() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(4);
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        // No supported_methods advertised → unsupported.
        let err = state
            .send_server_notification(&id, "RevealPath", "{}".into())
            .await
            .unwrap_err();
        assert!(matches!(err, ServerRequestError::UnsupportedMethod));
    }

    #[tokio::test]
    async fn send_server_notification_delivers_when_method_supported() {
        let state = make_state();
        let (tx, mut rx) = mpsc::channel(4);
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        state
            .update_supported_methods(&id, vec!["RevealPath".into()])
            .await;
        state
            .send_server_notification(&id, "RevealPath", "{\"p\":1}".into())
            .await
            .unwrap();
        let msg = rx.recv().await.unwrap();
        match msg.command {
            Some(channel_outbound::Command::ServerRequest(sr)) => {
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
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        // No supported_methods advertised → reject before dispatch.
        let result = state
            .send_server_request_and_await(
                &id,
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
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        state
            .update_supported_methods(&id, vec!["AskUser".into()])
            .await;
        // The awaited future borrows `self: &Arc<Self>`; move owned clones
        // into the spawned task so it satisfies the `'static` bound.
        let handle = tokio::spawn(async move {
            state
                .send_server_request_and_await(
                    &id,
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
        let id = state
            .mint_channel("row-ws".into(), "ws".into(), None, tx)
            .await;
        state
            .update_supported_methods(&id, vec!["AskUser".into()])
            .await;
        let state_for_await = state.clone();
        let id_for_await = id.clone();
        let handle = tokio::spawn(async move {
            state_for_await
                .send_server_request_and_await(
                    &id_for_await,
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
            "empty channel id should not match"
        );
        assert!(
            state
                .deliver_client_response(&id, "req-7", ServerRequestOutcome::Result("ok".into()))
                .await
        );
        let outcome = handle.await.unwrap().unwrap();
        match outcome {
            ServerRequestOutcome::Result(s) => assert_eq!(s, "ok"),
            ServerRequestOutcome::Error(_) => panic!("expected Result outcome"),
        }
    }

    #[tokio::test]
    async fn update_supported_methods_unknown_channel_is_false() {
        let state = make_state();
        assert!(
            !state
                .update_supported_methods("ghost", vec!["X".into()])
                .await
        );
    }
}
