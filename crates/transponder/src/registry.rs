//! Single-workspace conversation registry.
//!
//! The transponder is the sole author of its workspace's conversation
//! logs. This registry holds every minted conversation's in-memory log
//! plus its user-facing metadata, backed by a [`ConversationStoreFactory`]
//! (LocalFs on the conversation PVC by default). The registry is the
//! source of truth: a speculative `get_or_create` on a never-minted id
//! rebuilds an empty log but does NOT register it, so a previously-deleted
//! id can never resurrect into `list`/`owns`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::conversation::{ConversationLog, ConversationStoreFactory};

/// Fixed store sub-path. The transponder runs one workspace per pod (the
/// conversation PVC is already per-workspace), so a single constant
/// segment keeps the factory's `(workspace, conv_id)` API satisfied
/// without a workspace map.
const STORE_WORKSPACE: &str = "default";

/// Per-conversation metadata. Distinct from the log so freshly minted
/// (no-events-yet) conversations are registered before any append.
#[derive(Clone, Debug)]
struct ConversationMeta {
    /// Unix epoch milliseconds; 0 = registered but never touched. In-memory
    /// only — resets to 0 on restart when the registry rebuilds from disk.
    last_touched_ms: i64,
    /// User-facing name. Defaults to a short id stub at mint; mutable via
    /// `set_name`. Persisted to the `meta.json` sidecar.
    name: String,
}

/// First 8 chars of a conversation id — a compact default name so the
/// drawer shows something before the user renames it.
fn default_name_for_conversation(conv_id: &str) -> String {
    conv_id.chars().take(8).collect()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) struct ConversationRegistry {
    factory: Arc<dyn ConversationStoreFactory>,
    logs: RwLock<HashMap<String, Arc<RwLock<ConversationLog>>>>,
    meta: RwLock<HashMap<String, ConversationMeta>>,
    /// Cancellation token per in-flight turn, keyed by conversation_id. One
    /// turn per conversation at a time (the message loop is sequential), so a
    /// single token per id suffices. Registered at turn start, fired by
    /// `cancel`, cleared when the turn ends.
    turns: RwLock<HashMap<String, CancellationToken>>,
}

impl ConversationRegistry {
    pub(crate) fn new(factory: Arc<dyn ConversationStoreFactory>) -> Self {
        Self {
            factory,
            logs: RwLock::new(HashMap::new()),
            meta: RwLock::new(HashMap::new()),
            turns: RwLock::new(HashMap::new()),
        }
    }

    /// Register a fresh cancellation token for a starting turn and return a
    /// clone for the turn loop to race against. Replaces any stale token for
    /// the same conversation (previous turn already ended).
    pub(crate) async fn register_turn(&self, conv_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.turns
            .write()
            .await
            .insert(conv_id.to_string(), token.clone());
        token
    }

    /// Drop the in-flight-turn token once the turn ends, so a later `cancel`
    /// on an idle conversation reports no turn.
    pub(crate) async fn end_turn(&self, conv_id: &str) {
        self.turns.write().await.remove(conv_id);
    }

    /// Fire the in-flight turn's cancellation token. Returns true iff a turn
    /// was registered (in flight); false on an idle conversation.
    pub(crate) async fn cancel(&self, conv_id: &str) -> bool {
        match self.turns.write().await.remove(conv_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Load (or rebuild-on-miss) the log for `conv_id`. Does NOT register
    /// the id — the registry is truth, and a speculative lookup on a
    /// never-minted or deleted id must not resurrect it.
    pub(crate) async fn get_or_create(
        &self,
        conv_id: &str,
    ) -> Result<Arc<RwLock<ConversationLog>>, String> {
        {
            let logs = self.logs.read().await;
            if let Some(c) = logs.get(conv_id) {
                return Ok(c.clone());
            }
        }
        let mut logs = self.logs.write().await;
        if let Some(c) = logs.get(conv_id) {
            return Ok(c.clone());
        }
        let store = self.factory.make_store(STORE_WORKSPACE, conv_id);
        let log = ConversationLog::rebuild(store).await?;
        let arc = Arc::new(RwLock::new(log));
        logs.insert(conv_id.to_string(), arc.clone());
        Ok(arc)
    }

    /// Mint a fresh conversation: write its `meta.json` sidecar, then
    /// register it. Persist-first so a write failure leaves no phantom id
    /// in memory.
    pub(crate) async fn mint(&self) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let name = default_name_for_conversation(&id);
        let store = self.factory.make_store(STORE_WORKSPACE, &id);
        store.write_meta(&name).await?;
        self.meta.write().await.insert(
            id.clone(),
            ConversationMeta {
                last_touched_ms: now_ms(),
                name,
            },
        );
        Ok(id)
    }

    /// Persist a new name (caller enforces the length cap), then update the
    /// in-memory registry. No-op on an unknown id beyond the persisted
    /// sidecar write.
    pub(crate) async fn set_name(&self, conv_id: &str, new_name: &str) -> Result<(), String> {
        let store = self.factory.make_store(STORE_WORKSPACE, conv_id);
        store.write_meta(new_name).await?;
        if let Some(m) = self.meta.write().await.get_mut(conv_id) {
            m.name = new_name.to_string();
        }
        Ok(())
    }

    /// Permanently delete: wipe persisted events first, then evict the
    /// in-memory log + registry entry. On persist failure nothing is
    /// evicted, so the deletion is retryable.
    pub(crate) async fn delete(&self, conv_id: &str) -> Result<(), String> {
        let store = self.factory.make_store(STORE_WORKSPACE, conv_id);
        store.delete_all().await?;
        self.meta.write().await.remove(conv_id);
        self.logs.write().await.remove(conv_id);
        Ok(())
    }

    /// True when the registry knows this id. The log cache is NOT consulted
    /// — a rebuilt-but-unregistered (speculative or deleted) log must not
    /// look owned.
    pub(crate) async fn owns(&self, conv_id: &str) -> bool {
        self.meta.read().await.contains_key(conv_id)
    }

    /// Pull a known id to the top of MRU. No-op on unknown ids.
    pub(crate) async fn touch(&self, conv_id: &str) {
        if let Some(m) = self.meta.write().await.get_mut(conv_id) {
            m.last_touched_ms = now_ms();
        }
    }

    /// `(id, last_touched_ms, name)` for every registered conversation, in
    /// unspecified order (clients impose their own sort).
    pub(crate) async fn list_summaries(&self) -> Vec<(String, i64, String)> {
        self.meta
            .read()
            .await
            .iter()
            .map(|(id, m)| (id.clone(), m.last_touched_ms, m.name.clone()))
            .collect()
    }

    /// Restart recovery: walk `meta.json` sidecars on disk and seed the
    /// registry. `last_touched_ms` starts at 0 (disk carries no recency).
    /// Existing entries are never clobbered (a mint racing the walk wins).
    pub(crate) async fn rebuild_from_disk(&self) -> Result<(), String> {
        let pairs = self.factory.walk_conversations(STORE_WORKSPACE).await?;
        let count = pairs.len();
        let mut meta = self.meta.write().await;
        for (id, name) in pairs {
            meta.entry(id).or_insert(ConversationMeta {
                last_touched_ms: 0,
                name,
            });
        }
        tracing::info!(seeded = count, "seeded conversation registry from disk");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::test_support::{FailureModes, InjectableFactory};
    use crate::conversation::LocalFsFactory;
    use hangar_providers::types::{ContentBlock, Message};

    fn local_registry() -> (ConversationRegistry, std::path::PathBuf) {
        let root = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> =
            Arc::new(LocalFsFactory::new(root.clone()));
        (ConversationRegistry::new(factory), root)
    }

    fn failing_registry(modes: FailureModes) -> ConversationRegistry {
        ConversationRegistry::new(Arc::new(InjectableFactory(modes)))
    }

    fn user(text: &str) -> Message {
        Message {
            role: "user".into(),
            content: Some(ContentBlock::text_content(text)),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
        }
    }

    #[tokio::test]
    async fn mint_registers_with_default_name_and_persists_meta() {
        let (reg, root) = local_registry();
        let id = reg.mint().await.unwrap();
        assert_eq!(default_name_for_conversation(&id).chars().count(), 8);

        let summaries = reg.list_summaries().await;
        let (_, _, name) = summaries.iter().find(|(rid, _, _)| rid == &id).unwrap();
        assert_eq!(name, &default_name_for_conversation(&id));

        let meta_path = root.join(STORE_WORKSPACE).join(&id).join("meta.json");
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(meta_path).unwrap()).unwrap();
        assert_eq!(parsed["name"], default_name_for_conversation(&id));
    }

    #[tokio::test]
    async fn mint_persist_failure_leaves_registry_empty() {
        // Mutation target: insert into the registry before write_meta.
        let reg = failing_registry(FailureModes {
            write_meta: true,
            ..Default::default()
        });
        let err = reg
            .mint()
            .await
            .expect_err("persist failure must propagate");
        assert!(err.contains("write_meta"));
        assert!(reg.list_summaries().await.is_empty());
    }

    #[tokio::test]
    async fn get_or_create_does_not_register_unknown_id() {
        let (reg, _root) = local_registry();
        let _ = reg.get_or_create("never-minted").await.unwrap();
        assert!(!reg.owns("never-minted").await);
        assert!(reg.list_summaries().await.is_empty());
    }

    #[tokio::test]
    async fn delete_success_purges_registry_and_disk() {
        let (reg, root) = local_registry();
        let id = reg.mint().await.unwrap();
        let log = reg.get_or_create(&id).await.unwrap();
        log.write().await.append(user("hello")).await.unwrap();
        let dir = root.join(STORE_WORKSPACE).join(&id);
        assert!(dir.exists());

        reg.delete(&id).await.unwrap();
        assert!(!reg.owns(&id).await);
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn delete_persist_failure_keeps_registry_intact() {
        let reg = failing_registry(FailureModes {
            delete_all: true,
            ..Default::default()
        });
        let id = reg.mint().await.unwrap();
        let err = reg.delete(&id).await;
        assert!(err.is_err());
        assert!(reg.owns(&id).await);
    }

    #[tokio::test]
    async fn set_name_updates_registry_and_survives_rebuild() {
        let (reg, _root) = local_registry();
        let id = reg.mint().await.unwrap();
        reg.set_name(&id, "Quarterly review").await.unwrap();
        let after = reg.list_summaries().await;
        assert_eq!(
            after.iter().find(|(rid, _, _)| rid == &id).unwrap().2,
            "Quarterly review"
        );

        reg.rebuild_from_disk().await.unwrap();
        let after_restart = reg.list_summaries().await;
        assert_eq!(
            after_restart
                .iter()
                .find(|(rid, _, _)| rid == &id)
                .unwrap()
                .2,
            "Quarterly review"
        );
    }

    #[tokio::test]
    async fn rebuild_seeds_minted_then_deleted_does_not_resurrect() {
        let (reg, root) = local_registry();
        let kept = reg.mint().await.unwrap();
        let gone = reg.mint().await.unwrap();
        reg.delete(&gone).await.unwrap();

        // Fresh registry over the same root: only the surviving sidecar seeds.
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(root));
        let fresh = ConversationRegistry::new(factory);
        fresh.rebuild_from_disk().await.unwrap();

        let ids: Vec<String> = fresh
            .list_summaries()
            .await
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(ids, vec![kept]);
        assert!(!fresh.owns(&gone).await);
    }

    #[tokio::test]
    async fn touch_advances_known_id_and_ignores_unknown() {
        let (reg, _root) = local_registry();
        reg.touch("unknown").await;
        assert!(!reg.owns("unknown").await);

        let id = reg.mint().await.unwrap();
        let before = reg
            .list_summaries()
            .await
            .into_iter()
            .find(|(rid, _, _)| rid == &id)
            .unwrap()
            .1;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        reg.touch(&id).await;
        let after = reg
            .list_summaries()
            .await
            .into_iter()
            .find(|(rid, _, _)| rid == &id)
            .unwrap()
            .1;
        assert!(after >= before);
    }

    // ---- ACCEPTANCE (client-activity-ribs) ----
    // EARS: "When the transponder receives a CancelTurn for an in-flight turn,
    // it shall stop the turn's LLM stream and abandon its in-flight work."
    // The registry holds a per-conversation CancellationToken; the CancelTurn
    // handler fires it (plan 4a/4b). These pin the fire-the-right-token half;
    // consume_turn_stream_cancellable (turn.rs) pins the abandon half.

    #[tokio::test]
    async fn cancel_fires_the_in_flight_turns_token() {
        // A registered in-flight turn's token is triggered by cancel(), and
        // cancel reports that a turn WAS in flight.
        // Materiality: no-op the token.cancel() in the handler (or fire a
        // fresh token instead of the registered one) -> consume_turn_stream's
        // cancel arm never trips and the turn runs to completion.
        let (reg, _root) = local_registry();
        let id = reg.mint().await.unwrap();
        let token = reg.register_turn(&id).await;
        assert!(!token.is_cancelled());

        let was_in_flight = reg.cancel(&id).await;
        assert!(was_in_flight, "an in-flight turn is reported cancelled");
        assert!(token.is_cancelled(), "the in-flight turn's token must fire");
    }

    #[tokio::test]
    async fn cancel_of_idle_conversation_reports_no_turn() {
        // No turn registered -> cancel reports false (CancelTurnResponse
        // { cancelled: false }); nothing to abandon.
        // Materiality: return true unconditionally -> the client is told a
        // turn was cancelled when none was running.
        let (reg, _root) = local_registry();
        let id = reg.mint().await.unwrap();
        let was_in_flight = reg.cancel(&id).await;
        assert!(!was_in_flight);
    }
}
