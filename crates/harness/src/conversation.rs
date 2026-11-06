use async_trait::async_trait;
use chrono::Utc;
use proto_common::{content_block, image_block, ContentBlock, Message, ToolCall};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

/// On-disk conversation log filename. One append-only ND-JSON file per
/// conversation, one JSON record per line in append order.
const CONVERSATION_FILENAME: &str = "conversation.json";

/// Per-conversation metadata sidecar. Lives next to the conversation log in
/// the conversation directory; carries the user-facing name (default =
/// truncated id at mint time, mutable via `SetConversationName`). Written via
/// `<META_TMP_FILENAME>` + atomic rename so a crash between write+rename
/// never leaves a half-written `meta.json` visible to the startup walk.
const META_FILENAME: &str = "meta.json";
const META_TMP_FILENAME: &str = "meta.json.tmp";

/// Cap on the persisted conversation name. Enforced by the gRPC handler
/// (`SetConversationName`); the storage layer trusts its callers.
pub const MAX_CONVERSATION_NAME_CHARS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MetaFile {
    name: String,
    /// Opaque owner key stamped at mint. Empty for sidecars written before
    /// conversations were owned; such a conversation belongs to no owner and
    /// is therefore listed for none.
    #[serde(default)]
    owner: String,
}

/// Storage backend for conversation events. Two impls: LocalFs (the
/// default, writes to a directory on the controller's PVC) and S3 (writes
/// to the tenant's S3 prefix in Hetzner Object Storage / Versitygw).
///
/// All methods are async because S3 is fundamentally async; LocalFs's
/// blocking fs calls are wrapped in `spawn_blocking` to keep the runtime
/// responsive when the disk stalls.
#[async_trait]
pub trait ConversationStore: Send + Sync {
    /// Append a single event as one JSON line to the conversation log, in
    /// arrival order.
    async fn append_event(&self, entry: &LogEntry) -> Result<(), String>;
    /// Read every event in this conversation, in append order. Tolerant: a
    /// half-written trailing line (torn on a crash) is skipped, not fatal.
    async fn read_all(&self) -> Result<Vec<LogEntry>, String>;
    /// Permanently delete every event in this conversation. Used by
    /// `DeleteConversation`. No-op if the conversation has no
    /// persisted events.
    async fn delete_all(&self) -> Result<(), String>;
    /// Persist the conversation's user-facing name and its owner key to
    /// the sidecar (`meta.json`). Trusts the caller to have length-checked
    /// against [`MAX_CONVERSATION_NAME_CHARS`]; the gRPC handler is the
    /// single server-side gate. Writes via `meta.json.tmp` + atomic rename
    /// so concurrent readers never see a half-written file.
    async fn write_meta(&self, name: &str, owner: &str) -> Result<(), String>;
    /// Read the conversation's `(name, owner)` from the sidecar. Returns
    /// `Ok(None)` if the sidecar is absent (mint never ran, or this isn't a
    /// conversation directory); `Err` for malformed JSON.
    async fn read_meta(&self) -> Result<Option<(String, String)>, String>;
}

/// Factory that hands out a `ConversationStore` for a given
/// `(agent, conv_id)`. Constructed once at controller startup from the
/// configured backend (LocalFs or S3); each `AgentState` holds an
/// `Arc<dyn ConversationStoreFactory>` and asks for stores lazily as
/// conversations are first touched.
#[async_trait]
pub trait ConversationStoreFactory: Send + Sync {
    fn make_store(&self, agent: &str, conv_id: &str) -> Arc<dyn ConversationStore>;

    /// The on-disk directory for a conversation, when the backend is a local
    /// filesystem. The execution log (`execution.json` + `blobs/`) lives here
    /// alongside the conversation log, so the registry derives the
    /// per-conversation execution writer from this path. `None` for backends
    /// with no local directory (e.g. S3).
    fn conversation_dir(&self, agent: &str, conv_id: &str) -> Option<PathBuf>;

    /// Enumerate every conversation that has a `meta.json` under
    /// `agent`, as `(conv_id, name, owner)`. Used by the harness's
    /// startup walk to seed the in-memory registry from disk.
    /// Subdirectories without a `meta.json` (or with a malformed one) are
    /// skipped with a warning — they are either mid-mint races or stale
    /// fragments and must not crash boot.
    async fn walk_conversations(
        &self,
        agent: &str,
    ) -> Result<Vec<(String, String, String)>, String>;

    /// Enumerate every agent prefix that has any conversation
    /// directories on disk. Driver for the startup walk: each name is
    /// then passed to `walk_conversations` to recover the registry.
    /// Missing storage root → empty vec (first boot, nothing to rebuild).
    async fn list_agents(&self) -> Result<Vec<String>, String>;
}

/// Factory backed by a local directory. Each store writes to
/// `<root>/<agent>/<conv_id>/conversation.json`.
pub struct LocalFsFactory {
    root: PathBuf,
}

impl LocalFsFactory {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl ConversationStoreFactory for LocalFsFactory {
    fn make_store(&self, agent: &str, conv_id: &str) -> Arc<dyn ConversationStore> {
        Arc::new(LocalFsStore::new(self.root.join(agent).join(conv_id)))
    }

    fn conversation_dir(&self, agent: &str, conv_id: &str) -> Option<PathBuf> {
        Some(self.root.join(agent).join(conv_id))
    }

    async fn walk_conversations(
        &self,
        agent: &str,
    ) -> Result<Vec<(String, String, String)>, String> {
        let agent_dir = self.root.join(agent);
        let conv_ids = tokio::task::spawn_blocking({
            let dir = agent_dir.clone();
            move || -> Result<Vec<String>, String> {
                if !dir.exists() {
                    return Ok(Vec::new());
                }
                let mut out = Vec::new();
                for de in fs::read_dir(&dir)
                    .map_err(|e| format!("failed to read agent dir {}: {e}", dir.display()))?
                    .flatten()
                {
                    if !de.path().is_dir() {
                        continue;
                    }
                    if let Some(name) = de.file_name().to_str() {
                        out.push(name.to_string());
                    }
                }
                Ok(out)
            }
        })
        .await
        .map_err(|e| format!("walk_conversations join error: {e}"))??;

        let mut results = Vec::with_capacity(conv_ids.len());
        for conv_id in conv_ids {
            let store = self.make_store(agent, &conv_id);
            match store.read_meta().await {
                Ok(Some((name, owner))) => results.push((conv_id, name, owner)),
                Ok(None) => {
                    tracing::warn!(
                        conv_id = %conv_id,
                        "skipping conversation directory with no meta.json",
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        conv_id = %conv_id,
                        error = %e,
                        "skipping conversation with unreadable meta.json",
                    );
                }
            }
        }
        Ok(results)
    }

    async fn list_agents(&self) -> Result<Vec<String>, String> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
            if !root.exists() {
                return Ok(Vec::new());
            }
            let mut out = Vec::new();
            for de in fs::read_dir(&root)
                .map_err(|e| format!("failed to read storage root {}: {e}", root.display()))?
                .flatten()
            {
                if !de.path().is_dir() {
                    continue;
                }
                if let Some(name) = de.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
            Ok(out)
        })
        .await
        .map_err(|e| format!("list_agents join error: {e}"))?
    }
}

/// Local-filesystem store: one append-only `conversation.json` under
/// `log_dir/`, one JSON record per line. Lives behind a PVC in the controller
/// pod today.
pub struct LocalFsStore {
    log_dir: PathBuf,
}

impl LocalFsStore {
    pub fn new(log_dir: PathBuf) -> Self {
        Self { log_dir }
    }
}

#[async_trait]
impl ConversationStore for LocalFsStore {
    async fn append_event(&self, entry: &LogEntry) -> Result<(), String> {
        use std::io::Write;
        let path = self.log_dir.join(CONVERSATION_FILENAME);
        let log_dir = self.log_dir.clone();
        let mut line = serde_json::to_string(entry)
            .map_err(|e| format!("failed to serialize log entry: {e}"))?;
        line.push('\n');
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            fs::create_dir_all(&log_dir).map_err(|e| format!("failed to create log dir: {e}"))?;
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
            file.write_all(line.as_bytes())
                .map_err(|e| format!("failed to append event {}: {e}", path.display()))?;
            Ok(())
        })
        .await
        .map_err(|e| format!("append_event join error: {e}"))?
    }

    async fn read_all(&self) -> Result<Vec<LogEntry>, String> {
        let path = self.log_dir.join(CONVERSATION_FILENAME);
        tokio::task::spawn_blocking(move || -> Result<Vec<LogEntry>, String> {
            let contents = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
            };
            let mut out = Vec::new();
            for line in contents.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                // Tolerant reader: a half-written trailing line (torn on a
                // crash mid-append) fails to parse and is skipped, not fatal.
                match serde_json::from_str::<LogEntry>(line) {
                    Ok(entry) => out.push(entry),
                    Err(e) => {
                        tracing::warn!(error = %e, "skipping unparsable conversation log line");
                    }
                }
            }
            Ok(out)
        })
        .await
        .map_err(|e| format!("read_all join error: {e}"))?
    }

    async fn delete_all(&self) -> Result<(), String> {
        let log_dir = self.log_dir.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            match fs::remove_dir_all(&log_dir) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!(
                    "failed to delete conversation dir {}: {e}",
                    log_dir.display()
                )),
            }
        })
        .await
        .map_err(|e| format!("delete_all join error: {e}"))?
    }

    async fn write_meta(&self, name: &str, owner: &str) -> Result<(), String> {
        let payload = serde_json::to_vec(&MetaFile {
            name: name.to_string(),
            owner: owner.to_string(),
        })
        .map_err(|e| format!("failed to serialize meta.json: {e}"))?;
        let log_dir = self.log_dir.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            fs::create_dir_all(&log_dir).map_err(|e| {
                format!(
                    "failed to create conversation dir {}: {e}",
                    log_dir.display()
                )
            })?;
            let tmp = log_dir.join(META_TMP_FILENAME);
            let final_path = log_dir.join(META_FILENAME);
            fs::write(&tmp, &payload)
                .map_err(|e| format!("failed to write {}: {e}", tmp.display()))?;
            fs::rename(&tmp, &final_path).map_err(|e| {
                format!(
                    "failed to rename {} -> {}: {e}",
                    tmp.display(),
                    final_path.display()
                )
            })?;
            Ok(())
        })
        .await
        .map_err(|e| format!("write_meta join error: {e}"))?
    }

    async fn read_meta(&self) -> Result<Option<(String, String)>, String> {
        let path = self.log_dir.join(META_FILENAME);
        tokio::task::spawn_blocking(move || -> Result<Option<(String, String)>, String> {
            match fs::read(&path) {
                Ok(bytes) => {
                    let meta: MetaFile = serde_json::from_slice(&bytes)
                        .map_err(|e| format!("malformed {}: {e}", path.display()))?;
                    Ok(Some((meta.name, meta.owner)))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("failed to read {}: {e}", path.display())),
            }
        })
        .await
        .map_err(|e| format!("read_meta join error: {e}"))?
    }
}

/// Hex SHA-256 of a string. Used to fingerprint the system prompt an LLM
/// ran under so audits can compare against the canonical instructions files
/// without storing the prompt verbatim on every entry.
pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Strip a leading YAML frontmatter block from an instructions file and
/// return the body. The frontmatter is never parsed; it controls nothing.
///
/// Lines split on `\n`. One leading UTF-8 BOM is dropped from line 1, and one
/// trailing `\r` is removed from each line before it is compared. The file
/// has frontmatter iff line 1 is exactly `---`; the closer is the first later
/// line that is exactly `---` (the last line may close with or without a
/// trailing newline). The body is every byte after the closer line's `\n`,
/// empty if the closer ends the file. With no closer the input comes back
/// unchanged. There is no size limit.
pub fn strip_frontmatter(input: &str) -> String {
    let after_bom = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let mut lines = after_bom.split_inclusive('\n');
    let is_delimiter = |line: &str| {
        let line = line.strip_suffix('\n').unwrap_or(line);
        line.strip_suffix('\r').unwrap_or(line) == "---"
    };
    match lines.next() {
        Some(first) if is_delimiter(first) => {}
        _ => return input.to_string(),
    }
    if lines.by_ref().any(is_delimiter) {
        return lines.collect();
    }
    input.to_string()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AssistantAttribution {
    pub model: Option<String>,
    pub system_prompt_sha256: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct Entry {
    /// RFC 3339 timestamp at append time. Persisted through the
    /// `LogEntry.ts` field; preserved across `rebuild()` so `snapshot()`
    /// can report the original append moment to callers (e.g., the
    /// harness's `recent_turns` tool).
    ts: String,
    message: Message,
    attribution: AssistantAttribution,
}

/// Tail-of-log projection returned to tool callers and other read-side
/// consumers. Holds the proto `Message` directly — the harness gRPC handler
/// passes it straight onto the wire without conversion.
#[derive(Debug, Clone)]
pub struct EntrySnapshot {
    /// 1-indexed sequence in the conversation log.
    pub seq: u64,
    /// RFC 3339 timestamp from the original append.
    pub ts: String,
    pub message: Message,
}

pub struct ConversationLog {
    entries: Vec<Entry>,
    store: Arc<dyn ConversationStore>,
}

impl ConversationLog {
    /// Empty log backed by `store`.
    pub fn new(store: Arc<dyn ConversationStore>) -> Self {
        Self {
            entries: Vec::new(),
            store,
        }
    }

    /// Replay every persisted event from `store` into a new in-memory log.
    pub async fn rebuild(store: Arc<dyn ConversationStore>) -> Result<Self, String> {
        let persisted = store.read_all().await?;
        let entries = persisted
            .into_iter()
            .map(|log_entry| Entry {
                ts: log_entry.ts,
                message: Message {
                    role: log_entry.role,
                    content: log_entry.content,
                    tool_calls: log_entry.tool_calls,
                    tool_call_id: log_entry.tool_call_id,
                    is_error: log_entry.is_error,
                },
                attribution: AssistantAttribution {
                    model: log_entry.model,
                    system_prompt_sha256: log_entry.system_prompt_sha256,
                    warnings: log_entry.warnings,
                },
            })
            .collect();
        Ok(Self { entries, store })
    }

    pub async fn append(&mut self, message: Message) -> Result<(), String> {
        if message.role == "assistant" {
            return Err("append rejects role=\"assistant\"; use append_assistant".to_string());
        }
        let entry = Entry {
            ts: Utc::now().to_rfc3339(),
            message,
            attribution: AssistantAttribution::default(),
        };
        let log_entry = Self::entry_to_log_entry(&entry);
        self.store.append_event(&log_entry).await?;
        self.entries.push(entry);
        Ok(())
    }

    /// Append an assistant entry with attribution metadata (model that ran the
    /// call, hash of the system prompt the LLM was given). Use this from the
    /// LLM-result-streaming path; user and tool entries should keep using
    /// [`append`].
    ///
    /// Returns Err if the message has no text content and no tool calls —
    /// persisting an empty assistant entry poisons the conversation log
    /// (subsequent turns fail with API 400).
    pub async fn append_assistant(
        &mut self,
        message: Message,
        attribution: AssistantAttribution,
    ) -> Result<(), String> {
        if message.role != "assistant" {
            return Err(format!(
                "append_assistant requires role=\"assistant\", got \"{}\"",
                message.role
            ));
        }
        if message.content.is_empty() && message.tool_calls.is_empty() {
            return Err(
                "refusing to persist assistant message with no content and no tool_calls".into(),
            );
        }
        let entry = Entry {
            ts: Utc::now().to_rfc3339(),
            message,
            attribution,
        };
        let log_entry = Self::entry_to_log_entry(&entry);
        self.store.append_event(&log_entry).await?;
        self.entries.push(entry);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn history(&self) -> Vec<Message> {
        self.entries.iter().map(|e| e.message.clone()).collect()
    }

    pub fn attributions(&self) -> Vec<AssistantAttribution> {
        self.entries.iter().map(|e| e.attribution.clone()).collect()
    }

    pub fn history_for_provider(&self) -> Vec<Message> {
        self.entries.iter().map(|e| e.message.clone()).collect()
    }

    fn entry_to_log_entry(entry: &Entry) -> LogEntry {
        let content = if entry.message.role == "tool" {
            entry
                .message
                .content
                .iter()
                .map(|block| match &block.block {
                    // Redact produced-artifact image bytes from the persisted
                    // log; keep the media type so the entry stays meaningful.
                    Some(content_block::Block::Image(img)) => {
                        image_block(img.media_type.clone(), Vec::new())
                    }
                    // A file block is a reference (name/mime/size), not inline
                    // bytes, so it persists unchanged — like the old
                    // FileIncoming — as does text/thinking.
                    _ => block.clone(),
                })
                .collect()
        } else {
            entry.message.content.clone()
        };
        LogEntry {
            ts: entry.ts.clone(),
            role: entry.message.role.clone(),
            content,
            tool_calls: entry.message.tool_calls.clone(),
            tool_call_id: entry.message.tool_call_id.clone(),
            is_error: entry.message.is_error,
            model: entry.attribution.model.clone(),
            system_prompt_sha256: entry.attribution.system_prompt_sha256.clone(),
            warnings: entry.attribution.warnings.clone(),
        }
    }

    /// Tail-of-log projection for read-side consumers (the harness's
    /// `recent_turns` built-in tool today). Returns up to `limit` most
    /// recent entries in oldest-to-newest order along with the total
    /// log length at snapshot time; `None` or `Some(0)` returns the
    /// entire log. Callers pair `truncated = total_seq > entries.len()`
    /// to surface whether the head was clipped.
    pub fn snapshot(&self, limit: Option<usize>) -> ConversationSnapshot {
        let total = self.entries.len();
        let effective_limit = match limit {
            None | Some(0) => total,
            Some(n) => n.min(total),
        };
        let skip = total - effective_limit;
        let entries: Vec<EntrySnapshot> = self
            .entries
            .iter()
            .enumerate()
            .skip(skip)
            .map(|(idx, e)| EntrySnapshot {
                seq: (idx + 1) as u64,
                ts: e.ts.clone(),
                message: e.message.clone(),
            })
            .collect();
        ConversationSnapshot {
            entries,
            total_seq: total as u64,
        }
    }
}

/// Result of [`ConversationLog::snapshot`]. Carries the tail projection
/// and the full log length so the caller can report whether truncation
/// occurred.
#[derive(Debug, Clone)]
pub struct ConversationSnapshot {
    pub entries: Vec<EntrySnapshot>,
    pub total_seq: u64,
}

/// Test fixture: a `ConversationStore` / `ConversationStoreFactory` pair
/// that returns Ok by default and injects an `Err(...)` on whichever
/// method(s) the caller flags. Used by the registry tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Pick which store methods should inject a failure. All `false` =
    /// fully permissive store (returns Ok with empty payloads).
    #[derive(Default, Clone, Copy, Debug)]
    pub(crate) struct FailureModes {
        pub(crate) read_all: bool,
        pub(crate) append_event: bool,
        pub(crate) delete_all: bool,
        pub(crate) write_meta: bool,
    }

    /// Factory that hands out [`InjectableStore`] instances pre-armed
    /// with the same `FailureModes`. `walk_conversations` and
    /// `list_agents` always return empty so the startup walk is a
    /// no-op.
    pub(crate) struct InjectableFactory(pub(crate) FailureModes);

    /// Per-conversation store that respects [`FailureModes`].
    pub(crate) struct InjectableStore(pub(crate) FailureModes);

    #[async_trait]
    impl ConversationStoreFactory for InjectableFactory {
        fn make_store(&self, _agent: &str, _conv_id: &str) -> Arc<dyn ConversationStore> {
            Arc::new(InjectableStore(self.0))
        }
        fn conversation_dir(&self, _agent: &str, _conv_id: &str) -> Option<PathBuf> {
            None
        }
        async fn walk_conversations(
            &self,
            _agent: &str,
        ) -> Result<Vec<(String, String, String)>, String> {
            Ok(vec![])
        }
        async fn list_agents(&self) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl ConversationStore for InjectableStore {
        async fn append_event(&self, _entry: &LogEntry) -> Result<(), String> {
            if self.0.append_event {
                Err("injected append_event failure".into())
            } else {
                Ok(())
            }
        }
        async fn read_all(&self) -> Result<Vec<LogEntry>, String> {
            if self.0.read_all {
                Err("injected read_all failure".into())
            } else {
                Ok(vec![])
            }
        }
        async fn delete_all(&self) -> Result<(), String> {
            if self.0.delete_all {
                Err("injected delete_all failure".into())
            } else {
                Ok(())
            }
        }
        async fn write_meta(&self, _name: &str, _owner: &str) -> Result<(), String> {
            if self.0.write_meta {
                Err("injected write_meta failure".into())
            } else {
                Ok(())
            }
        }
        async fn read_meta(&self) -> Result<Option<(String, String)>, String> {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto_common::{content_text, text_block, text_content};
    use tempfile::TempDir;

    fn text_msg(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: text_content(text),
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        }
    }

    #[tokio::test]
    async fn append_rejects_caller_supplied_assistant_role() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
        let err = log
            .append(text_msg("assistant", "forged"))
            .await
            .expect_err("append must reject caller-supplied assistant role");
        assert!(
            err.contains("assistant"),
            "error message should name the rejected role, got: {err}"
        );
        assert!(
            log.history().is_empty(),
            "rejected append must not mutate history"
        );
    }

    #[tokio::test]
    async fn append_writes_one_ndjson_record_per_object_to_conversation_json() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        log.append(text_msg("user", "Hello")).await.unwrap();
        log.append_assistant(
            text_msg("assistant", "Hi there"),
            AssistantAttribution::default(),
        )
        .await
        .unwrap();

        assert_eq!(log.history().len(), 2);
        assert_eq!(log.history()[0].role, "user");
        assert_eq!(log.history()[1].role, "assistant");

        // One append-only conversation.json, one JSON record per line (append,
        // not overwrite: both records survive).
        let conv = tmp.path().join("conversation.json");
        assert!(
            conv.is_file(),
            "the conversation log is a single conversation.json"
        );
        let text = fs::read_to_string(&conv).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "one appended object -> one ND-JSON line");
        let parsed1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed1["role"], "user");
        let parsed2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(parsed2["role"], "assistant");
    }

    #[tokio::test]
    async fn rebuild_restores_history_from_log() {
        let tmp = TempDir::new().unwrap();

        {
            let mut log =
                ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
            log.append(text_msg("user", "First")).await.unwrap();
            log.append_assistant(
                Message {
                    role: "assistant".into(),
                    content: vec![],
                    tool_calls: vec![ToolCall {
                        id: "tc-001".into(),
                        name: "bash".into(),
                        input_json: r#"{"command": "ls"}"#.into(),
                    }],
                    tool_call_id: None,
                    is_error: None,
                },
                AssistantAttribution::default(),
            )
            .await
            .unwrap();
            log.append(Message {
                role: "tool".into(),
                content: text_content("ls output"),
                tool_calls: vec![],
                tool_call_id: Some("tc-001".into()),
                is_error: None,
            })
            .await
            .unwrap();
            log.append_assistant(
                text_msg("assistant", "Second"),
                AssistantAttribution::default(),
            )
            .await
            .unwrap();
            log.append(text_msg("user", "Third")).await.unwrap();
        }

        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        let history = rebuilt.history();
        assert_eq!(history.len(), 5);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[1].role, "assistant");
        assert_eq!(history[1].tool_calls.len(), 1);
        assert_eq!(history[1].tool_calls[0].name, "bash");
        assert_eq!(history[2].role, "tool");
        assert_eq!(history[2].tool_call_id.as_deref(), Some("tc-001"));
        assert_eq!(history[3].role, "assistant");
        assert_eq!(history[4].role, "user");
    }

    #[tokio::test]
    async fn rebuild_empty_dir_returns_empty_log() {
        let tmp = TempDir::new().unwrap();
        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        assert!(rebuilt.history().is_empty());
    }

    #[tokio::test]
    async fn append_assistant_rejects_empty_message() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        let msg = Message {
            role: "assistant".into(),
            content: vec![],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        };
        let err = log
            .append_assistant(msg, AssistantAttribution::default())
            .await
            .expect_err("empty assistant message must be rejected");
        assert!(
            err.contains("no content and no tool_calls"),
            "error should name the reason, got: {err}"
        );
        assert!(
            log.history().is_empty(),
            "rejected append must not mutate history"
        );
    }

    #[tokio::test]
    async fn rebuild_skips_a_corrupted_trailing_line() {
        // The reader tolerates a half-written trailing line (torn on a crash
        // mid-append): the intact record before the tear still rebuilds.
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path()).unwrap();
        std::fs::write(
            tmp.path().join("conversation.json"),
            "{\"ts\":\"t\",\"role\":\"user\",\"content\":[{\"block\":{\"Text\":{\"text\":\"ok\"}}}]}\n{\"ts\":\"t\",\"role\":\"user\",\"conte",
        )
        .unwrap();
        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .expect("a torn trailing line must not fail the rebuild");
        assert_eq!(
            rebuilt.history().len(),
            1,
            "the intact record still rebuilds"
        );
        assert_eq!(content_text(&rebuilt.history()[0].content), "ok");
    }

    #[tokio::test]
    async fn assistant_attribution_round_trips_through_rebuild() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log =
                ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
            log.append(text_msg("user", "hi")).await.unwrap();
            log.append_assistant(
                text_msg("assistant", "hello back"),
                AssistantAttribution {
                    model: Some("default".into()),
                    system_prompt_sha256: Some(sha256_hex("You are helpful.")),
                    warnings: vec![],
                },
            )
            .await
            .unwrap();
            log.append_assistant(
                text_msg("assistant", "second reply"),
                AssistantAttribution {
                    model: Some("anthropic.haiku".into()),
                    system_prompt_sha256: Some(sha256_hex("You are alice.")),
                    warnings: vec![],
                },
            )
            .await
            .unwrap();
        }

        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        let attrs = rebuilt.attributions();
        assert_eq!(attrs.len(), 3);

        // User entry has no attribution.
        assert!(attrs[0].model.is_none());
        assert!(attrs[0].system_prompt_sha256.is_none());

        // First assistant: model + hash.
        assert_eq!(attrs[1].model.as_deref(), Some("default"));
        assert_eq!(
            attrs[1].system_prompt_sha256.as_deref(),
            Some(sha256_hex("You are helpful.").as_str())
        );

        // Second assistant: model + hash.
        assert_eq!(attrs[2].model.as_deref(), Some("anthropic.haiku"));
        assert_eq!(
            attrs[2].system_prompt_sha256.as_deref(),
            Some(sha256_hex("You are alice.").as_str())
        );
    }

    /// Frontmatter has no size limit: a 64 KiB block is stripped whole, so the
    /// harness never meets frontmatter the uploader admitted but it cannot
    /// strip. A build that keeps a scan limit passes the input through here.
    #[tokio::test]
    async fn strip_frontmatter_strips_a_64_kib_frontmatter() {
        let mut input = String::from("---\ndescription: big\n");
        while input.len() < 64 * 1024 {
            input.push_str("# pad pad pad pad pad pad pad pad\n");
        }
        input.push_str("---\nbody line\n");
        assert_eq!(strip_frontmatter(&input), "body line\n");
    }

    /// No closer means no frontmatter: the input comes back unchanged.
    #[tokio::test]
    async fn strip_frontmatter_without_a_closer_returns_the_input_unchanged() {
        let input = "---\ntitle: x\nbody";
        assert_eq!(strip_frontmatter(input), input);
    }

    /// Line 1 must be exactly `---`; a trailing space, a fourth dash or any
    /// other suffix is not an opener.
    #[tokio::test]
    async fn strip_frontmatter_with_a_bad_opener_returns_the_input_unchanged() {
        for input in [
            "--- \nk: v\n---\nbody",
            "----\nk: v\n---\nbody",
            "---x\nk: v\n---\nbody",
        ] {
            assert_eq!(strip_frontmatter(input), input, "{input:?}");
        }
    }

    /// One leading UTF-8 BOM is dropped from line 1, so `BOM---` opens, and
    /// the body carries no BOM.
    #[tokio::test]
    async fn strip_frontmatter_accepts_a_bom_before_the_opener_and_drops_it() {
        let body = strip_frontmatter("\u{FEFF}---\nk: v\n---\nbody");
        assert_eq!(body, "body");
        assert!(!body.starts_with('\u{FEFF}'));
    }

    /// One trailing `\r` is removed before each compare, so a CRLF file's
    /// opener and closer count.
    #[tokio::test]
    async fn strip_frontmatter_strips_a_crlf_frontmatter() {
        assert_eq!(strip_frontmatter("---\r\nk: v\r\n---\r\nbody"), "body");
    }

    /// A closer on the last line with no trailing newline ends the file, so
    /// the body is empty.
    #[tokio::test]
    async fn strip_frontmatter_with_a_closer_ending_the_file_returns_an_empty_body() {
        assert_eq!(strip_frontmatter("---\nk: v\n---"), "");
    }

    // `list_agents` returns child directory names and skips plain files.
    #[tokio::test]
    async fn list_agents_returns_dir_names_excluding_files() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("alpha")).unwrap();
        fs::write(tmp.path().join("note.txt"), b"x").unwrap();
        let factory = LocalFsFactory::new(tmp.path().to_path_buf());
        let agents = factory.list_agents().await.unwrap();
        assert_eq!(agents, vec!["alpha".to_string()]);
    }

    #[tokio::test]
    async fn assistant_attribution_warnings_round_trip() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log =
                ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
            log.append_assistant(
                text_msg("assistant", "ok"),
                AssistantAttribution {
                    model: Some("haiku".into()),
                    system_prompt_sha256: None,
                    warnings: vec!["model".into(), "messages".into()],
                },
            )
            .await
            .unwrap();
        }
        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        let attrs = rebuilt.attributions();
        assert_eq!(
            attrs[0].warnings,
            vec!["model".to_string(), "messages".to_string()]
        );
    }

    #[tokio::test]
    async fn assistant_attribution_warnings_default_empty_when_omitted() {
        let tmp = TempDir::new().unwrap();
        // A record without the `warnings` field; serde defaults to empty.
        std::fs::write(
            tmp.path().join("conversation.json"),
            "{\"ts\":\"t\",\"role\":\"assistant\",\"content\":[{\"block\":{\"Text\":{\"text\":\"hi\"}}}],\"model\":\"haiku\"}\n",
        )
        .unwrap();
        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        let attrs = rebuilt.attributions();
        assert!(attrs[0].warnings.is_empty());
    }

    #[tokio::test]
    async fn is_empty_reflects_actual_entry_count() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
        assert!(log.is_empty(), "fresh log should be empty");

        log.append(text_msg("user", "hi")).await.unwrap();

        assert!(!log.is_empty(), "log with 1 entry must not report empty");
        assert_eq!(log.len(), 1);
    }

    // --- snapshot() / ts preservation ---

    fn fresh_log() -> ConversationLog {
        let tmp = TempDir::new().unwrap().keep();
        ConversationLog::new(Arc::new(LocalFsStore::new(tmp)))
    }

    #[tokio::test]
    async fn snapshot_limit_takes_the_tail_and_unset_zero_or_oversize_take_all() {
        let mut log = fresh_log();
        log.append(text_msg("user", "msg-0")).await.unwrap();
        for n in 1..5 {
            log.append(text_msg("user", &format!("msg-{n}")))
                .await
                .unwrap();
        }

        // None returns every entry, in order.
        let snap = log.snapshot(None);
        assert_eq!(snap.entries.len(), 5);
        assert_eq!(snap.total_seq, 5);
        for (i, entry) in snap.entries.iter().enumerate() {
            assert_eq!(entry.seq, i as u64 + 1);
            assert_eq!(content_text(&entry.message.content), format!("msg-{i}"));
        }

        // Plan says: Some(0) → same as None (no limit). The proto wire
        // contract treats unset / 0 as "no limit"; conversation.rs
        // mirrors that.
        assert_eq!(log.snapshot(Some(0)).entries.len(), 5);

        let snap = log.snapshot(Some(2));
        assert_eq!(snap.entries.len(), 2);
        assert_eq!(snap.total_seq, 5);
        // Tail: msg-3 (seq 4) + msg-4 (seq 5).
        assert_eq!(snap.entries[0].seq, 4);
        assert_eq!(snap.entries[1].seq, 5);
        assert_eq!(content_text(&snap.entries[0].message.content), "msg-3");
        assert_eq!(content_text(&snap.entries[1].message.content), "msg-4");

        let snap = log.snapshot(Some(100));
        assert_eq!(snap.entries.len(), 5, "a limit past the length returns all");
        assert_eq!(snap.total_seq, 5);
    }

    #[tokio::test]
    async fn snapshot_ts_round_trips_through_rebuild() {
        let tmp = TempDir::new().unwrap().keep();
        let store: Arc<dyn ConversationStore> = Arc::new(LocalFsStore::new(tmp.clone()));
        let mut log = ConversationLog::new(store.clone());
        log.append(text_msg("user", "persisted")).await.unwrap();
        let original_ts = log.snapshot(None).entries[0].ts.clone();

        // Drop in-memory log, rebuild from disk via a fresh store handle.
        drop(log);
        let rebuilt = ConversationLog::rebuild(store).await.unwrap();
        let rebuilt_ts = rebuilt.snapshot(None).entries[0].ts.clone();
        assert_eq!(
            rebuilt_ts, original_ts,
            "ts must survive rebuild — pinned because the LogEntry.ts \
             field is the only persisted timestamp source"
        );
    }

    #[tokio::test]
    async fn write_meta_roundtrip_preserves_name_and_owner() {
        let tmp = TempDir::new().unwrap();
        let store = LocalFsStore::new(tmp.path().to_path_buf());
        store
            .write_meta("Quarterly review", "caleb-phone")
            .await
            .unwrap();
        assert_eq!(
            store.read_meta().await.unwrap(),
            Some(("Quarterly review".to_string(), "caleb-phone".to_string())),
        );
    }

    #[tokio::test]
    async fn walk_conversations_returns_id_name_and_owner_per_meta_json() {
        let tmp = TempDir::new().unwrap();
        let factory = LocalFsFactory::new(tmp.path().to_path_buf());

        factory
            .make_store("ag", "alpha")
            .write_meta("Alpha chat", "caleb-phone")
            .await
            .unwrap();
        factory
            .make_store("ag", "beta")
            .write_meta("Beta chat", "caleb-laptop")
            .await
            .unwrap();

        // A bare subdirectory without meta.json — must be skipped, not error.
        fs::create_dir_all(tmp.path().join("ag").join("gamma")).unwrap();
        // A stray file at the agent root — must not be enumerated.
        fs::write(tmp.path().join("ag").join("readme.txt"), "noise").unwrap();

        let mut walked = factory.walk_conversations("ag").await.unwrap();
        walked.sort();
        assert_eq!(
            walked,
            vec![
                (
                    "alpha".to_string(),
                    "Alpha chat".to_string(),
                    "caleb-phone".to_string()
                ),
                (
                    "beta".to_string(),
                    "Beta chat".to_string(),
                    "caleb-laptop".to_string()
                ),
            ],
        );
    }

    // A produced artifact reaches the conversation-history write site as a
    // tool-role image block. Its base64 bytes must not be persisted to
    // conversation.json, and no reference to them either; the sibling caption text
    // and the tool_call_id survive so the tool entry stays meaningful. rebuild
    // reads the entry back with the image block present but its `data` emptied.
    //
    // Stop emptying `data` and the base64-absent and empty-data assertions red; a
    // too-broad edit that drops the whole block or the caption reds the
    // caption/tool_call_id survival assertions.
    #[tokio::test]
    async fn tool_role_image_bytes_are_stripped_from_conversation_history() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(LocalFsStore::new(tmp.path().to_path_buf()));
        let mut log = ConversationLog::new(store);

        // A distinctive byte pattern: if it survives to disk the bytes leaked.
        // Serde writes proto `bytes` as a comma-joined number array, so the
        // marker is that serialized form.
        let payload: Vec<u8> = vec![222, 173, 190, 239];
        let marker = "222,173,190,239";
        let tool_msg = Message {
            role: "tool".into(),
            content: vec![
                image_block("image/png".into(), payload),
                text_block("rendered chart caption".into()),
            ],
            tool_calls: vec![],
            tool_call_id: Some("tc-render-1".into()),
            is_error: None,
        };
        log.append(tool_msg).await.unwrap();

        let conv = tmp.path().join("conversation.json");
        let text = fs::read_to_string(&conv).expect("conversation.json exists");
        assert!(
            !text.contains(marker),
            "a produced artifact's bytes must not be inlined into conversation.json, got {text:?}"
        );
        assert!(
            text.contains("rendered chart caption"),
            "the sibling caption text survives the strip, got {text:?}"
        );
        assert!(
            text.contains("tc-render-1"),
            "the tool_call_id survives the strip, got {text:?}"
        );

        // The persisted entry keeps the image block, with its bytes emptied.
        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        let msg = rebuilt.history().into_iter().next().expect("one entry");
        let blocks = msg.content;
        let image_data = blocks.iter().find_map(|b| match b.block.as_ref() {
            Some(content_block::Block::Image(img)) => Some(img.data.clone()),
            _ => None,
        });
        assert!(
            image_data
                .as_ref()
                .expect("the persisted tool entry keeps the image block")
                .is_empty(),
            "the persisted tool image block carries an emptied data field, got {image_data:?}"
        );
        assert!(
            blocks.iter().any(|b| matches!(
                b.block.as_ref(),
                Some(content_block::Block::Text(t)) if t.text == "rendered chart caption"
            )),
            "the caption text block survives in the rebuilt entry"
        );
    }

    // A user-supplied input image stays byte-inline in conversation history,
    // unchanged. The role gate strips only tool-role images.
    #[tokio::test]
    async fn user_input_image_bytes_stay_inline_in_conversation_history() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(LocalFsStore::new(tmp.path().to_path_buf()));
        let mut log = ConversationLog::new(store);

        let payload: Vec<u8> = vec![222, 173, 190, 239];
        let marker = "222,173,190,239";
        let user_msg = Message {
            role: "user".into(),
            content: vec![image_block("image/png".into(), payload)],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        };
        log.append(user_msg).await.unwrap();

        let conv = tmp.path().join("conversation.json");
        let text = fs::read_to_string(&conv).expect("conversation.json exists");
        assert!(
            text.contains(marker),
            "a user-supplied input image stays byte-inline in conversation.json, got {text:?}"
        );
    }
}
