//! Single-workspace conversation registry.
//!
//! The harness is the sole author of its workspace's conversation
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
use crate::execution_log::{ExecutionLogWriter, LocalFsExecutionLog};

/// Fixed store sub-path. The harness runs one workspace per pod (the
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
    /// Opaque owner key stamped by the caller at mint. The harness never
    /// parses it, never derives authorization from it, and never learns
    /// what it names — it is an equality key. Persisted to the `meta.json`
    /// sidecar so ownership survives a harness roll.
    owner: String,
}

/// First 8 chars of a conversation id — a compact default name so the
/// drawer shows something before the user renames it.
fn default_name_for_conversation(conv_id: &str) -> String {
    conv_id.chars().take(8).collect()
}

/// Reject any conversation id that is not a well-formed UUID before it
/// becomes a path segment. Conversation ids are bare UUIDs minted by
/// [`ConversationRegistry::mint`]; a client-supplied value that is not one
/// (a `..` traversal, a separator, an empty string) is refused here rather
/// than sanitized, so no toolset- or client-supplied string ever names a
/// first-party directory.
fn validate_conversation_id(conv_id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(conv_id)
        .map(|_| ())
        .map_err(|_| format!("conversation id is not a well-formed UUID: {conv_id}"))
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
    /// Per-conversation execution-log writer, derived once from the factory's
    /// conversation directory and cached so every append to one conversation's
    /// `execution.json` shares the same serialization mutex — the model-turn
    /// path and the app-run path never interleave a line.
    exec_logs: RwLock<HashMap<String, Arc<dyn ExecutionLogWriter>>>,
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
            exec_logs: RwLock::new(HashMap::new()),
            meta: RwLock::new(HashMap::new()),
            turns: RwLock::new(HashMap::new()),
        }
    }

    /// The execution-log writer for a conversation, derived from the factory's
    /// conversation directory (`<root>/default/<conv_id>/`) and cached so
    /// concurrent appends share one serialization mutex. `None` when the
    /// backend has no local directory (e.g. S3). The id is validated so an
    /// unvalidated string never names the directory.
    pub(crate) async fn execution_log_for(
        &self,
        conv_id: &str,
    ) -> Option<Arc<dyn ExecutionLogWriter>> {
        validate_conversation_id(conv_id).ok()?;
        {
            let logs = self.exec_logs.read().await;
            if let Some(l) = logs.get(conv_id) {
                return Some(l.clone());
            }
        }
        let dir = self.factory.conversation_dir(STORE_WORKSPACE, conv_id)?;
        let mut logs = self.exec_logs.write().await;
        let entry = logs
            .entry(conv_id.to_string())
            .or_insert_with(|| Arc::new(LocalFsExecutionLog::new(dir, conv_id.to_string())));
        Some(entry.clone())
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
        validate_conversation_id(conv_id)?;
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
    pub(crate) async fn mint(&self, owner: &str) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let name = default_name_for_conversation(&id);
        let store = self.factory.make_store(STORE_WORKSPACE, &id);
        store.write_meta(&name, owner).await?;
        self.meta.write().await.insert(
            id.clone(),
            ConversationMeta {
                last_touched_ms: now_ms(),
                name,
                owner: owner.to_string(),
            },
        );
        Ok(id)
    }

    /// Persist a new name (caller enforces the length cap), then update the
    /// in-memory registry. No-op on an unknown id beyond the persisted
    /// sidecar write.
    pub(crate) async fn set_name(&self, conv_id: &str, new_name: &str) -> Result<(), String> {
        validate_conversation_id(conv_id)?;
        // The owner is rewritten verbatim: a rename must never orphan a
        // conversation from the row that minted it.
        let owner = self
            .meta
            .read()
            .await
            .get(conv_id)
            .map(|m| m.owner.clone())
            .unwrap_or_default();
        let store = self.factory.make_store(STORE_WORKSPACE, conv_id);
        store.write_meta(new_name, &owner).await?;
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

    /// The opaque owner key a conversation was minted under, if known.
    pub(crate) async fn owner_of(&self, conv_id: &str) -> Option<String> {
        self.meta.read().await.get(conv_id).map(|m| m.owner.clone())
    }

    /// True when the registry knows this id AND it was minted by `owner`.
    /// An equality check on an opaque string; the harness reads nothing
    /// into either side.
    pub(crate) async fn owned_by(&self, conv_id: &str, owner: &str) -> bool {
        self.meta
            .read()
            .await
            .get(conv_id)
            .is_some_and(|m| m.owner == owner)
    }

    /// Pull a known id to the top of MRU. No-op on unknown ids.
    pub(crate) async fn touch(&self, conv_id: &str) {
        if let Some(m) = self.meta.write().await.get_mut(conv_id) {
            m.last_touched_ms = now_ms();
        }
    }

    /// `(id, last_touched_ms, name)` for every conversation `owner` minted,
    /// in unspecified order (clients impose their own sort). Another
    /// owner's conversations are not enumerated.
    pub(crate) async fn list_summaries(&self, owner: &str) -> Vec<(String, i64, String)> {
        self.meta
            .read()
            .await
            .iter()
            .filter(|(_, m)| m.owner == owner)
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
        for (id, name, owner) in pairs {
            meta.entry(id).or_insert(ConversationMeta {
                last_touched_ms: 0,
                name,
                owner,
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
    use proto_common::{text_content, Message};

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
            content: text_content(text),
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        }
    }

    #[tokio::test]
    async fn mint_registers_with_default_name_and_persists_meta() {
        let (reg, root) = local_registry();
        let id = reg.mint("test-owner").await.unwrap();
        assert_eq!(default_name_for_conversation(&id).chars().count(), 8);

        let summaries = reg.list_summaries("test-owner").await;
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
            .mint("test-owner")
            .await
            .expect_err("persist failure must propagate");
        assert!(err.contains("write_meta"));
        assert!(reg.list_summaries("test-owner").await.is_empty());
    }

    // Path-safety: the conversation id is a bare UUID; a value that is
    // not a well-formed UUID is rejected at the directory-construction boundary,
    // never sanitized. This REPLACES the prior
    // `get_or_create_does_not_register_unknown_id`: a non-UUID id is no longer
    // quietly rebuilt-but-unregistered — get_or_create's own tightened check
    // rejects it outright, while a freshly minted (bare UUID) id still loads.
    //
    // Materiality: drop the UUID check in `get_or_create` and "never-minted"
    // rebuilds an empty log and returns Ok -> `is_err()` reds. Regress `mint`
    // to a non-bare id (e.g. a `<workspace>.<uuid>` prefix) and the parse +
    // accept assertions red.
    //
    // Pins the reject-non-UUID behavior (first-party path segments only) AND that
    // the real mint->load flow the Flutter client depends on survives the
    // tightening — a stub that accepts everything fails the first assertion.
    #[tokio::test]
    async fn get_or_create_rejects_a_non_uuid_id() {
        let (reg, _root) = local_registry();
        // "never-minted" is not a well-formed UUID: it must be rejected, not
        // rebuilt into a log cached under an unvalidated directory name.
        let rejected = reg.get_or_create("never-minted").await;
        assert!(
            rejected.is_err(),
            "a conversation id that is not a well-formed UUID must be rejected by get_or_create"
        );
        assert!(!reg.owns("never-minted").await);
        assert!(reg.list_summaries("test-owner").await.is_empty());

        // The empty-id-mints-fresh flow is preserved: a freshly minted id is a
        // well-formed (bare) UUID and get_or_create accepts it.
        let fresh = reg.mint("test-owner").await.unwrap();
        assert!(
            uuid::Uuid::parse_str(&fresh).is_ok(),
            "mint yields a bare, well-formed UUID, got {fresh:?}"
        );
        reg.get_or_create(&fresh)
            .await
            .expect("a minted UUID is a valid path segment and loads");
    }

    // Path-safety at the directory-construction boundary itself (`make_store`).
    // `set_name` is a non-`get_or_create` caller that funnels a client-supplied
    // conv_id straight to `make_store` then a write, so it pins that the boundary
    // — not just `get_or_create` — refuses a non-first-party segment.
    //
    // Materiality: skip the UUID check at the make_store boundary and join the
    // raw string, and `set_name("../escape-marker-not-a-uuid", ..)` writes
    // `root/escape-marker-not-a-uuid/meta.json` (`..` climbs out of the `default`
    // workspace dir) and returns Ok -> both the `is_err` and the "no escape dir"
    // assertions red.
    //
    // Asserts validate-and-reject (Err, not a silently sanitized path) plus the
    // observable filesystem invariant "a rejected id never becomes a directory".
    #[tokio::test]
    async fn make_store_boundary_rejects_a_non_uuid_conversation_id() {
        let (reg, root) = local_registry();
        let evil = "../escape-marker-not-a-uuid";
        let res = reg.set_name(evil, "x").await;
        assert!(
            res.is_err(),
            "a conv_id that is not a well-formed UUID must be rejected at the \
             directory-construction boundary, never written"
        );
        let escaped = root.join("escape-marker-not-a-uuid");
        assert!(
            !escaped.exists(),
            "a rejected id never becomes a directory: nothing may be created at {}",
            escaped.display()
        );
    }

    // Every execution-log record carries the conversation_id of the conversation
    // whose log it lands in, so the on-disk record is self-describing (resolution,
    // or any audit, can read the owning conversation off the record itself).
    //
    // Materiality: the id is threaded from `execution_log_for` ->
    // `LocalFsExecutionLog::new` -> `frame_to_record`, so the persisted line
    // carries it. A mutant that stamps an empty string (`conversation_id:
    // String::new()`) or the wrong id (e.g. reusing `call_id`) reds the
    // exact-value assert. The test drives the production seam that binds a
    // conversation id to its writer (`execution_log_for`), never a direct `::new`
    // call, so it reds semantically (wrong on-disk content), not structurally.
    #[tokio::test]
    async fn execution_record_is_stamped_with_its_conversation_id() {
        use proto_common::tool_result_frame::Frame;
        use proto_common::ToolResultFrame;

        let (reg, root) = local_registry();
        let conv_id = reg.mint("test-owner").await.unwrap();
        let writer = reg
            .execution_log_for(&conv_id)
            .await
            .expect("a local-fs conversation has an execution-log writer");

        let frame = ToolResultFrame {
            frame: Some(Frame::Stdout("hello".into())),
        };
        writer
            .append_frame("call-1", &frame)
            .await
            .expect("append the frame to the conversation's execution log");

        let exec_json = root
            .join(STORE_WORKSPACE)
            .join(&conv_id)
            .join("execution.json");
        let text = std::fs::read_to_string(&exec_json).unwrap_or_else(|e| {
            panic!("execution.json must exist at {}: {e}", exec_json.display())
        });
        let line = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .expect("the appended frame is one ND-JSON record line");
        let record: serde_json::Value =
            serde_json::from_str(line).expect("each execution.json line is JSON");
        assert_eq!(
            record["conversation_id"],
            serde_json::Value::String(conv_id.clone()),
            "each execution-log record is stamped with its owning conversation_id, got {record}"
        );
    }

    #[tokio::test]
    async fn delete_success_purges_registry_and_disk() {
        let (reg, root) = local_registry();
        let id = reg.mint("test-owner").await.unwrap();
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
        let id = reg.mint("test-owner").await.unwrap();
        let err = reg.delete(&id).await;
        assert!(err.is_err());
        assert!(reg.owns(&id).await);
    }

    #[tokio::test]
    async fn set_name_updates_registry_and_survives_rebuild() {
        let (reg, _root) = local_registry();
        let id = reg.mint("test-owner").await.unwrap();
        reg.set_name(&id, "Quarterly review").await.unwrap();
        let after = reg.list_summaries("test-owner").await;
        assert_eq!(
            after.iter().find(|(rid, _, _)| rid == &id).unwrap().2,
            "Quarterly review"
        );

        reg.rebuild_from_disk().await.unwrap();
        let after_restart = reg.list_summaries("test-owner").await;
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
        let kept = reg.mint("test-owner").await.unwrap();
        let gone = reg.mint("test-owner").await.unwrap();
        reg.delete(&gone).await.unwrap();

        // Fresh registry over the same root: only the surviving sidecar seeds.
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(root));
        let fresh = ConversationRegistry::new(factory);
        fresh.rebuild_from_disk().await.unwrap();

        let ids: Vec<String> = fresh
            .list_summaries("test-owner")
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

        let id = reg.mint("test-owner").await.unwrap();
        let before = reg
            .list_summaries("test-owner")
            .await
            .into_iter()
            .find(|(rid, _, _)| rid == &id)
            .unwrap()
            .1;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        reg.touch(&id).await;
        let after = reg
            .list_summaries("test-owner")
            .await
            .into_iter()
            .find(|(rid, _, _)| rid == &id)
            .unwrap()
            .1;
        assert!(after >= before);
    }

    // The registry holds a per-conversation CancellationToken; the CancelTurn
    // handler fires it to stop the turn's LLM stream and abandon its in-flight
    // work. These pin the fire-the-right-token half;
    // consume_turn_stream_cancellable (turn.rs) pins the abandon half.

    #[tokio::test]
    async fn cancel_fires_the_in_flight_turns_token() {
        // A registered in-flight turn's token is triggered by cancel(), and
        // cancel reports that a turn WAS in flight.
        // Materiality: no-op the token.cancel() in the handler (or fire a
        // fresh token instead of the registered one) -> consume_turn_stream_cancellable's
        // cancel arm never trips and the turn runs to completion.
        let (reg, _root) = local_registry();
        let id = reg.mint("test-owner").await.unwrap();
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
        let id = reg.mint("test-owner").await.unwrap();
        let was_in_flight = reg.cancel(&id).await;
        assert!(!was_in_flight);
    }

    // Cancelling a turn more than once is a safe no-op: no panic and no
    // duplicate terminal emission.
    #[tokio::test]
    async fn double_cancel_of_in_flight_turn_is_a_safe_no_op() {
        // Cancel an in-flight turn, then cancel the SAME conversation again.
        // The first cancel reports true and fires the token; the second must
        // NOT report another in-flight cancel (it would be a duplicate terminal
        // emission downstream) and must not panic. The shared token stays
        // cancelled throughout.
        //
        // Materiality: make `cancel` re-insert the token, fire without removing,
        // or otherwise leave the id registered after the first call -> the
        // second `cancel` returns true, telling the caller a second turn was
        // cancelled and driving a duplicate terminal Cancelled emission. That
        // flips the `assert!(!second)` below to red. (A double-fire panic in a
        // non-idempotent token would also red by crashing the test.)
        let (reg, _root) = local_registry();
        let id = reg.mint("test-owner").await.unwrap();
        let token = reg.register_turn(&id).await;

        let first = reg.cancel(&id).await;
        assert!(first, "first cancel of an in-flight turn reports true");
        assert!(token.is_cancelled(), "first cancel fires the token");

        // The second cancel is the load-bearing case: it must be a no-op.
        let second = reg.cancel(&id).await;
        assert!(
            !second,
            "a second cancel must be a safe no-op, not a duplicate terminal cancel"
        );
        assert!(
            token.is_cancelled(),
            "the turn stays cancelled after the redundant second cancel"
        );
    }

    // --- Conversation owner ------------------------------------------------
    //
    // The relay stamps an opaque owner string on the conversation at mint and
    // passes it back on every conversation-scoped RPC. The harness never parses
    // it, never derives authorization from it, and never learns the grants
    // table — it is an equality key and nothing else. Authorization stays at
    // the relay; what lives here is the durable record of who minted what.
    //
    // The owner is threaded through `ConversationMeta` and the `meta.json`
    // sidecar, so the stamp survives a harness roll.

    const ROW_A: &str = "caleb-phone";
    const ROW_B: &str = "caleb-laptop";

    /// `list_summaries` returns only the asking owner's conversations. Two rows
    /// in the same workspace is the only shape a single harness ever sees, since
    /// a harness *is* one workspace.
    ///
    /// Return the unfiltered map and `caleb-phone` sees `caleb-laptop`'s
    /// conversation drawer. Filter on the wrong side of the comparison and it
    /// sees only the other row's. Neither is visible at the relay, because the
    /// relay is not the thing enumerating.
    #[tokio::test]
    async fn list_summaries_returns_only_the_asking_owners_conversations() {
        let (reg, _root) = local_registry();
        let mine = reg.mint(ROW_A).await.unwrap();
        let theirs = reg.mint(ROW_B).await.unwrap();

        let ids: Vec<String> = reg
            .list_summaries(ROW_A)
            .await
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(ids, vec![mine.clone()]);

        let ids: Vec<String> = reg
            .list_summaries(ROW_B)
            .await
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(ids, vec![theirs]);

        // An owner that minted nothing sees nothing — not everything.
        assert!(reg.list_summaries("dad-telegram").await.is_empty());
    }

    /// The owner survives the harness roll, which means it lives on disk in the
    /// `meta.json` sidecar beside the name, not only in the in-memory
    /// `ConversationMeta`.
    ///
    /// Hold the owner in memory only and every conversation in the tenant
    /// becomes unowned at the first harness restart. Ownership then fails *open*
    /// cluster-wide with no relay-side symptom and no log line. This is the only
    /// test that observes it.
    #[tokio::test]
    async fn the_owner_stamp_survives_rebuild_from_disk() {
        let (reg, root) = local_registry();
        let mine = reg.mint(ROW_A).await.unwrap();
        let _theirs = reg.mint(ROW_B).await.unwrap();

        // Fresh registry over the same root: the harness pod was replaced.
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(root));
        let fresh = ConversationRegistry::new(factory);
        fresh.rebuild_from_disk().await.unwrap();

        let ids: Vec<String> = fresh
            .list_summaries(ROW_A)
            .await
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(
            ids,
            vec![mine],
            "after a roll each row must still see exactly its own conversations"
        );
        assert_eq!(
            fresh.list_summaries(ROW_B).await.len(),
            1,
            "the other row's conversation must survive too, still owned"
        );
    }
}
