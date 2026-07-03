use async_trait::async_trait;
use chrono::Utc;
use hangar_providers::types::{ContentBlock, Message, ToolCall};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

/// On-disk event file prefix and pad width. Files are named
/// `event-NNNNNN.json` so alphabetical listing == chronological order, and
/// 6 digits gives ~1M events per conversation (plenty).
const EVENT_FILE_PREFIX: &str = "event-";
const EVENT_FILE_SUFFIX: &str = ".json";
const EVENT_SEQ_WIDTH: usize = 6;

/// Per-conversation metadata sidecar. Lives next to event files in the
/// conversation directory; carries the user-facing name (default = truncated
/// id at mint time, mutable via `SetConversationName`). Written via
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
}

fn event_filename(seq: usize) -> String {
    format!(
        "{EVENT_FILE_PREFIX}{seq:0width$}{EVENT_FILE_SUFFIX}",
        width = EVENT_SEQ_WIDTH
    )
}

/// Parse a sequence number out of an `event-NNNNNN.json` filename. Returns
/// None for any other name shape so rebuild ignores unrelated files.
fn parse_event_seq(filename: &str) -> Option<usize> {
    let rest = filename.strip_prefix(EVENT_FILE_PREFIX)?;
    let seq_str = rest.strip_suffix(EVENT_FILE_SUFFIX)?;
    seq_str.parse().ok()
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
    /// Write a single event at the given 1-indexed sequence position.
    async fn write_event(&self, seq: usize, entry: &LogEntry) -> Result<(), String>;
    /// Read every event in this conversation, in sequence order.
    async fn read_all(&self) -> Result<Vec<LogEntry>, String>;
    /// Delete the event at the given sequence (used by `truncate`).
    /// No-op if the event doesn't exist.
    async fn delete_event(&self, seq: usize) -> Result<(), String>;
    /// Permanently delete every event in this conversation. Used by
    /// `DeleteConversation`. No-op if the conversation has no
    /// persisted events.
    async fn delete_all(&self) -> Result<(), String>;
    /// Persist the conversation's user-facing name to the sidecar
    /// (`meta.json`). Trusts the caller to have length-checked against
    /// [`MAX_CONVERSATION_NAME_CHARS`]; the gRPC handler is the single
    /// server-side gate. Writes via `meta.json.tmp` + atomic rename so
    /// concurrent readers never see a half-written file.
    async fn write_meta(&self, name: &str) -> Result<(), String>;
    /// Read the conversation's name from the sidecar. Returns `Ok(None)`
    /// if the sidecar is absent (mint never ran, or this isn't a
    /// conversation directory); `Err` for malformed JSON.
    async fn read_meta(&self) -> Result<Option<String>, String>;
}

/// Factory that hands out a `ConversationStore` for a given
/// `(workspace, conv_id)`. Constructed once at controller startup from the
/// configured backend (LocalFs or S3); each `WorkspaceState` holds an
/// `Arc<dyn ConversationStoreFactory>` and asks for stores lazily as
/// conversations are first touched.
#[async_trait]
pub trait ConversationStoreFactory: Send + Sync {
    fn make_store(&self, workspace: &str, conv_id: &str) -> Arc<dyn ConversationStore>;

    /// Enumerate every conversation that has a `meta.json` under
    /// `workspace`. Used by the controller's startup walk to seed the
    /// in-memory registry from disk. Subdirectories without a `meta.json`
    /// (or with a malformed one) are skipped with a warning — they are
    /// either mid-mint races or stale fragments and must not crash boot.
    async fn walk_conversations(&self, workspace: &str) -> Result<Vec<(String, String)>, String>;

    /// Enumerate every workspace prefix that has any conversation
    /// directories on disk. Driver for the startup walk: each name is
    /// then passed to `walk_conversations` to recover the registry.
    /// Missing storage root → empty vec (first boot, nothing to rebuild).
    async fn list_workspaces(&self) -> Result<Vec<String>, String>;
}

/// Factory backed by a local directory. Each store writes to
/// `<root>/<workspace>/<conv_id>/event-NNNNNN.json`.
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
    fn make_store(&self, workspace: &str, conv_id: &str) -> Arc<dyn ConversationStore> {
        Arc::new(LocalFsStore::new(self.root.join(workspace).join(conv_id)))
    }

    async fn walk_conversations(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let workspace_dir = self.root.join(workspace);
        let conv_ids = tokio::task::spawn_blocking({
            let dir = workspace_dir.clone();
            move || -> Result<Vec<String>, String> {
                if !dir.exists() {
                    return Ok(Vec::new());
                }
                let mut out = Vec::new();
                for de in fs::read_dir(&dir)
                    .map_err(|e| format!("failed to read workspace dir {}: {e}", dir.display()))?
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
            let store = self.make_store(workspace, &conv_id);
            match store.read_meta().await {
                Ok(Some(name)) => results.push((conv_id, name)),
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

    async fn list_workspaces(&self) -> Result<Vec<String>, String> {
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
        .map_err(|e| format!("list_workspaces join error: {e}"))?
    }
}

/// Local-filesystem store: one JSON file per event under `log_dir/`.
/// Lives behind a PVC in the controller pod today.
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
    async fn write_event(&self, seq: usize, entry: &LogEntry) -> Result<(), String> {
        let path = self.log_dir.join(event_filename(seq));
        let log_dir = self.log_dir.clone();
        let json = serde_json::to_string(entry)
            .map_err(|e| format!("failed to serialize log entry: {e}"))?;
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            fs::create_dir_all(&log_dir).map_err(|e| format!("failed to create log dir: {e}"))?;
            fs::write(&path, json)
                .map_err(|e| format!("failed to write event {}: {e}", path.display()))?;
            Ok(())
        })
        .await
        .map_err(|e| format!("write_event join error: {e}"))?
    }

    async fn read_all(&self) -> Result<Vec<LogEntry>, String> {
        let log_dir = self.log_dir.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<LogEntry>, String> {
            let mut events: Vec<(usize, PathBuf)> = Vec::new();
            if !log_dir.exists() {
                return Ok(Vec::new());
            }
            for de in fs::read_dir(&log_dir)
                .map_err(|e| format!("failed to read log dir: {e}"))?
                .flatten()
            {
                let path = de.path();
                if !path.is_file() {
                    continue;
                }
                let name = de.file_name().to_string_lossy().to_string();
                if let Some(seq) = parse_event_seq(&name) {
                    events.push((seq, path));
                }
            }
            events.sort_by_key(|(seq, _)| *seq);
            let mut out = Vec::with_capacity(events.len());
            for (_, path) in events {
                let contents = fs::read_to_string(&path)
                    .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
                let log_entry: LogEntry = serde_json::from_str(&contents)
                    .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
                out.push(log_entry);
            }
            Ok(out)
        })
        .await
        .map_err(|e| format!("read_all join error: {e}"))?
    }

    async fn delete_event(&self, seq: usize) -> Result<(), String> {
        let path = self.log_dir.join(event_filename(seq));
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("failed to delete {}: {e}", path.display())),
            }
        })
        .await
        .map_err(|e| format!("delete_event join error: {e}"))?
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

    async fn write_meta(&self, name: &str) -> Result<(), String> {
        let payload = serde_json::to_vec(&MetaFile {
            name: name.to_string(),
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

    async fn read_meta(&self) -> Result<Option<String>, String> {
        let path = self.log_dir.join(META_FILENAME);
        tokio::task::spawn_blocking(move || -> Result<Option<String>, String> {
            match fs::read(&path) {
                Ok(bytes) => {
                    let meta: MetaFile = serde_json::from_slice(&bytes)
                        .map_err(|e| format!("malformed {}: {e}", path.display()))?;
                    Ok(Some(meta.name))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("failed to read {}: {e}", path.display())),
            }
        })
        .await
        .map_err(|e| format!("read_meta join error: {e}"))?
    }
}

/// Convert a YAML value into a JSON object map. Returns None for non-mapping
/// values (the operator/principal will see no params override take effect).
fn yaml_value_to_json_object(
    v: &serde_yaml::Value,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if !v.is_mapping() {
        return None;
    }
    serde_json::to_value(v).ok().and_then(|jv| match jv {
        serde_json::Value::Object(map) => Some(map),
        _ => None,
    })
}

/// Hex SHA-256 of a string. Used to fingerprint the system prompt an LLM
/// ran under so audits can compare against canonical files in Mainframe
/// without storing the prompt verbatim on every entry.
pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Frontmatter fields the runtime cares about. Other YAML fields are ignored.
#[derive(Debug, Default, Clone)]
pub struct Frontmatter {
    pub model: Option<String>,
    pub params: Option<serde_json::Map<String, serde_json::Value>>,
}

const FRONTMATTER_SCAN_LIMIT: usize = 4 * 1024;

/// Parse YAML frontmatter from a system prompt string.
///
/// Recognizes a leading `---\n` (or `---\r\n`) block, optionally preceded by a
/// UTF-8 BOM. Returns the body (everything after the closing `---\n`) and any
/// extracted fields. The closing `---` must appear within the first 4 KiB; if
/// it doesn't, the function returns the original input unchanged and an empty
/// frontmatter. If YAML parsing fails, same — original input + empty
/// frontmatter, no error.
///
/// `model` is treated as a string. Non-string values are ignored (the body
/// is still stripped if frontmatter delimiters parse cleanly).
pub fn strip_frontmatter(input: &str) -> (String, Frontmatter) {
    let bytes = input.as_bytes();
    let start = if bytes.starts_with(b"\xEF\xBB\xBF") {
        3
    } else {
        0
    };
    let after_bom = &input[start..];

    let opener_len = if after_bom.starts_with("---\n") {
        4
    } else if after_bom.starts_with("---\r\n") {
        5
    } else {
        return (input.to_string(), Frontmatter::default());
    };

    let scan_end = (after_bom.len()).min(FRONTMATTER_SCAN_LIMIT);
    let scan_region = &after_bom[opener_len..scan_end];

    // Find a line containing exactly "---" (followed by \n, \r\n, or end).
    let mut closer_offset: Option<(usize, usize)> = None;
    let mut line_start = 0usize;
    for (idx, b) in scan_region.bytes().enumerate() {
        if b == b'\n' {
            let line = &scan_region[line_start..idx];
            let trimmed = line.strip_suffix('\r').unwrap_or(line);
            if trimmed == "---" {
                closer_offset = Some((line_start, idx + 1));
                break;
            }
            line_start = idx + 1;
        }
    }
    // Also handle a closer at EOF without trailing newline (within the scan region).
    if closer_offset.is_none()
        && scan_region.len() < FRONTMATTER_SCAN_LIMIT - opener_len
        && scan_region[line_start..]
            .strip_suffix('\r')
            .unwrap_or(&scan_region[line_start..])
            == "---"
    {
        closer_offset = Some((line_start, scan_region.len()));
    }

    let (yaml_end, body_start_in_region) = match closer_offset {
        Some(o) => o,
        None => return (input.to_string(), Frontmatter::default()),
    };

    let yaml_text = &scan_region[..yaml_end];
    let body = &after_bom[opener_len + body_start_in_region..];

    let fm = match serde_yaml::from_str::<serde_yaml::Value>(yaml_text) {
        Ok(serde_yaml::Value::Mapping(map)) => Frontmatter {
            model: map
                .get(serde_yaml::Value::String("model".into()))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            params: map
                .get(serde_yaml::Value::String("params".into()))
                .and_then(yaml_value_to_json_object),
        },
        Ok(_) => Frontmatter::default(),
        Err(e) => {
            tracing::debug!(error = %e, "system_prompt frontmatter failed to parse; passing through");
            return (input.to_string(), Frontmatter::default());
        }
    };

    (body.to_string(), fm)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
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
    /// transponder's `recent_turns` tool).
    ts: String,
    message: Message,
    tag: Option<String>,
    attribution: AssistantAttribution,
}

/// Tail-of-log projection returned to tool callers and other read-side
/// consumers. Plain Rust types (no protobuf dependency); the
/// `hangar-controller::grpc` handler converts to wire types.
#[derive(Debug, Clone)]
pub struct EntrySnapshot {
    /// 1-indexed sequence in the conversation log.
    pub seq: u64,
    /// RFC 3339 timestamp from the original append.
    pub ts: String,
    pub message: Message,
    pub tag: Option<String>,
}

const DELEGATE_TAG_PREFIX: &str = "delegate:";

fn entry_in_scope(entry: &Entry, scope: HistoryScope<'_>) -> bool {
    match scope {
        HistoryScope::Orchestrator => !entry
            .tag
            .as_deref()
            .is_some_and(|t| t.starts_with(DELEGATE_TAG_PREFIX)),
        HistoryScope::Delegate(call_id) => {
            entry.tag.as_deref() == Some(format!("{DELEGATE_TAG_PREFIX}{call_id}").as_str())
        }
    }
}

/// Conversation log tag for a turn entry. Delegate turns become
/// `delegate:<correlation_id>`; orchestrator turns are untagged.
pub fn derive_tag(
    role: Option<hangar_proto::TurnRole>,
    correlation_id: Option<&str>,
) -> Option<String> {
    use hangar_proto::TurnRole;
    match role {
        Some(TurnRole::Delegate) => correlation_id.map(|id| format!("{DELEGATE_TAG_PREFIX}{id}")),
        _ => None,
    }
}

/// Scope for [`ConversationLog::history_for_provider`]. Drives which tagged
/// entries are visible to the LLM prompt being built.
#[derive(Debug, Clone, Copy)]
pub enum HistoryScope<'a> {
    /// Orchestrator (or untagged agent) view: hide all delegate-scoped entries
    /// and any system-agent-internal entries.
    Orchestrator,
    /// Delegate view scoped to a specific call_id. Show only that delegate's
    /// own entries; everything else is hidden.
    Delegate(&'a str),
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
                tag: log_entry.tag,
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
        self.append_tagged(message, None).await
    }

    pub async fn append_tagged(
        &mut self,
        message: Message,
        tag: Option<String>,
    ) -> Result<(), String> {
        if message.role == "assistant" {
            return Err(
                "append_tagged rejects role=\"assistant\"; use append_assistant_tagged".to_string(),
            );
        }
        let entry = Entry {
            ts: Utc::now().to_rfc3339(),
            message,
            tag,
            attribution: AssistantAttribution::default(),
        };
        let seq = self.entries.len() + 1;
        let log_entry = Self::entry_to_log_entry(&entry);
        self.store.write_event(seq, &log_entry).await?;
        self.entries.push(entry);
        Ok(())
    }

    /// Append an assistant entry with attribution metadata (model that ran the
    /// call, hash of the system prompt the LLM was given, optional agent name
    /// for delegate calls). Use this from the LLM-result-streaming path; user
    /// and tool entries should keep using [`append_tagged`].
    ///
    /// Returns Err if the message has no text content and no tool calls —
    /// persisting an empty assistant entry poisons the conversation log
    /// (subsequent turns fail with API 400).
    pub async fn append_assistant_tagged(
        &mut self,
        message: Message,
        tag: Option<String>,
        attribution: AssistantAttribution,
    ) -> Result<(), String> {
        if message.role != "assistant" {
            return Err(format!(
                "append_assistant_tagged requires role=\"assistant\", got \"{}\"",
                message.role
            ));
        }
        if message.content.is_none() && message.tool_calls.is_none() {
            return Err(
                "refusing to persist assistant message with no content and no tool_calls".into(),
            );
        }
        let entry = Entry {
            ts: Utc::now().to_rfc3339(),
            message,
            tag,
            attribution,
        };
        let seq = self.entries.len() + 1;
        let log_entry = Self::entry_to_log_entry(&entry);
        self.store.write_event(seq, &log_entry).await?;
        self.entries.push(entry);
        Ok(())
    }

    pub async fn append_many_tagged(
        &mut self,
        messages: Vec<Message>,
        tag: Option<String>,
    ) -> Result<(), String> {
        for message in messages {
            self.append_tagged(message, tag.clone()).await?;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub async fn truncate(&mut self, len: usize) {
        if len >= self.entries.len() {
            return;
        }
        let old_len = self.entries.len();
        self.entries.truncate(len);
        // Delete events that are no longer part of the kept history.
        // Event seqs are 1-indexed; the surviving entries are seq 1..=len.
        for seq in (len + 1)..=old_len {
            if let Err(e) = self.store.delete_event(seq).await {
                tracing::error!(seq, "failed to delete truncated event: {e}");
            }
        }
    }

    pub fn history(&self) -> Vec<Message> {
        self.entries.iter().map(|e| e.message.clone()).collect()
    }

    pub fn tags(&self) -> Vec<Option<String>> {
        self.entries.iter().map(|e| e.tag.clone()).collect()
    }

    pub fn attributions(&self) -> Vec<AssistantAttribution> {
        self.entries.iter().map(|e| e.attribution.clone()).collect()
    }

    /// Most recent assistant entry's `attribution.model` within `scope`.
    /// Used by frontmatter `model: inherit` to pick up the model the previous
    /// turn in this thread ran under. Returns None if no prior assistant turn
    /// in scope has a model attribution.
    pub fn last_assistant_model(&self, scope: HistoryScope<'_>) -> Option<String> {
        self.entries
            .iter()
            .rev()
            .filter(|e| entry_in_scope(e, scope))
            .find(|e| e.message.role == "assistant" && e.attribution.model.is_some())
            .and_then(|e| e.attribution.model.clone())
    }

    pub fn history_for_provider(&self, scope: HistoryScope<'_>) -> Vec<Message> {
        self.entries
            .iter()
            .filter(|e| entry_in_scope(e, scope))
            .map(|e| e.message.clone())
            .collect()
    }

    fn entry_to_log_entry(entry: &Entry) -> LogEntry {
        LogEntry {
            ts: entry.ts.clone(),
            role: entry.message.role.clone(),
            content: entry.message.content.clone(),
            tool_calls: entry.message.tool_calls.clone(),
            tool_call_id: entry.message.tool_call_id.clone(),
            is_error: entry.message.is_error,
            tag: entry.tag.clone(),
            model: entry.attribution.model.clone(),
            system_prompt_sha256: entry.attribution.system_prompt_sha256.clone(),
            warnings: entry.attribution.warnings.clone(),
        }
    }

    /// Tail-of-log projection for read-side consumers (the transponder's
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
                tag: e.tag.clone(),
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
/// method(s) the caller flags. Subsumes the per-test fixtures that used
/// to live in `state.rs` and `tests/grpc_integration.rs`.
///
/// `#[doc(hidden)]` (not gated by `#[cfg(test)]`) so the integration
/// test crate at `tests/grpc_integration.rs` can reach it — integration
/// tests compile against the library's non-test build of the lib.
#[doc(hidden)]
pub mod test_support {
    use super::*;

    /// Pick which store methods should inject a failure. All `false` =
    /// fully permissive store (returns Ok with empty payloads).
    #[derive(Default, Clone, Copy, Debug)]
    pub struct FailureModes {
        pub read_all: bool,
        pub write_event: bool,
        pub delete_all: bool,
        pub write_meta: bool,
    }

    /// Factory that hands out [`InjectableStore`] instances pre-armed
    /// with the same `FailureModes`. `walk_conversations` and
    /// `list_workspaces` always return empty so the startup walk is a
    /// no-op.
    pub struct InjectableFactory(pub FailureModes);

    /// Per-conversation store that respects [`FailureModes`].
    pub struct InjectableStore(pub FailureModes);

    #[async_trait]
    impl ConversationStoreFactory for InjectableFactory {
        fn make_store(&self, _workspace: &str, _conv_id: &str) -> Arc<dyn ConversationStore> {
            Arc::new(InjectableStore(self.0))
        }
        async fn walk_conversations(
            &self,
            _workspace: &str,
        ) -> Result<Vec<(String, String)>, String> {
            Ok(vec![])
        }
        async fn list_workspaces(&self) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl ConversationStore for InjectableStore {
        async fn write_event(&self, _seq: usize, _entry: &LogEntry) -> Result<(), String> {
            if self.0.write_event {
                Err("injected write_event failure".into())
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
        async fn delete_event(&self, _seq: usize) -> Result<(), String> {
            Ok(())
        }
        async fn delete_all(&self) -> Result<(), String> {
            if self.0.delete_all {
                Err("injected delete_all failure".into())
            } else {
                Ok(())
            }
        }
        async fn write_meta(&self, _name: &str) -> Result<(), String> {
            if self.0.write_meta {
                Err("injected write_meta failure".into())
            } else {
                Ok(())
            }
        }
        async fn read_meta(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hangar_providers::types::content_text;
    use tempfile::TempDir;

    fn text_msg(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: Some(ContentBlock::text_content(text)),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
        }
    }

    #[tokio::test]
    async fn new_log_starts_empty() {
        let tmp = TempDir::new().unwrap();
        let log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
        assert!(log.history().is_empty());
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
    async fn append_adds_to_history_and_writes_event_per_object() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        log.append(text_msg("user", "Hello")).await.unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "Hi there"),
            None,
            AssistantAttribution::default(),
        )
        .await
        .unwrap();

        assert_eq!(log.history().len(), 2);
        assert_eq!(log.history()[0].role, "user");
        assert_eq!(log.history()[1].role, "assistant");

        let e1 = tmp.path().join("event-000001.json");
        let e2 = tmp.path().join("event-000002.json");
        assert!(
            e1.exists(),
            "event-000001.json must exist after first append"
        );
        assert!(
            e2.exists(),
            "event-000002.json must exist after second append"
        );

        // Each file contains exactly one JSON object (no trailing newline,
        // no ndjson framing).
        let parsed1: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&e1).unwrap()).unwrap();
        assert_eq!(parsed1["role"], "user");
        let parsed2: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&e2).unwrap()).unwrap();
        assert_eq!(parsed2["role"], "assistant");
    }

    #[tokio::test]
    async fn event_filename_zero_pads_to_six_digits() {
        // Pin the on-disk filename shape so alphabetical listing == sequence
        // order. A mutant that shrinks the width would land >999999 files
        // out of order.
        assert_eq!(event_filename(1), "event-000001.json");
        assert_eq!(event_filename(42), "event-000042.json");
        assert_eq!(event_filename(123456), "event-123456.json");
    }

    #[tokio::test]
    async fn local_fs_delete_event_treats_missing_file_as_success() {
        // Catches `replace == with !=` on the NotFound branch in
        // LocalFsStore::delete_event — without this, a missing file would
        // surface as Err instead of Ok.
        let tmp = TempDir::new().unwrap();
        let store = LocalFsStore::new(tmp.path().to_path_buf());
        // No event ever written; delete should still succeed.
        store
            .delete_event(42)
            .await
            .expect("deleting a non-existent event must be Ok");
    }

    #[tokio::test]
    async fn parse_event_seq_rejects_non_event_files() {
        assert_eq!(parse_event_seq("event-000001.json"), Some(1));
        assert_eq!(parse_event_seq("event-000042.json"), Some(42));
        assert!(parse_event_seq("conversation.ndjson").is_none());
        assert!(parse_event_seq("README.md").is_none());
        assert!(parse_event_seq("event-NaN.json").is_none());
        assert!(parse_event_seq("event-1.txt").is_none());
    }

    #[tokio::test]
    async fn rebuild_reads_event_files_in_sequence_order() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log =
                ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
            log.append(text_msg("user", "alpha")).await.unwrap();
            log.append_assistant_tagged(
                text_msg("assistant", "beta"),
                None,
                AssistantAttribution::default(),
            )
            .await
            .unwrap();
            log.append(text_msg("user", "gamma")).await.unwrap();
        }
        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        let h = rebuilt.history();
        assert_eq!(h.len(), 3);
        assert_eq!(content_text(&h[0].content), Some("alpha"));
        assert_eq!(content_text(&h[1].content), Some("beta"));
        assert_eq!(content_text(&h[2].content), Some("gamma"));
    }

    #[tokio::test]
    async fn rebuild_ignores_unrelated_files_in_log_dir() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log =
                ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
            log.append(text_msg("user", "real")).await.unwrap();
        }
        // Drop a stray file that doesn't match the event-NNNNNN.json shape.
        fs::write(tmp.path().join("README.md"), "ignore me").unwrap();
        fs::write(tmp.path().join("conversation.ndjson"), "{}").unwrap();

        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        assert_eq!(
            rebuilt.history().len(),
            1,
            "only real events should rebuild"
        );
    }

    #[tokio::test]
    async fn rebuild_restores_history_from_log() {
        let tmp = TempDir::new().unwrap();

        {
            let mut log =
                ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
            log.append(text_msg("user", "First")).await.unwrap();
            log.append_assistant_tagged(
                text_msg("assistant", "Second"),
                None,
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
        assert_eq!(rebuilt.history().len(), 3);
        assert_eq!(rebuilt.history()[0].role, "user");
        assert_eq!(rebuilt.history()[1].role, "assistant");
        assert_eq!(rebuilt.history()[2].role, "user");
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
    async fn tool_result_message_round_trips() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        let msg = Message {
            role: "tool".into(),
            content: Some(ContentBlock::text_content("ls output")),
            tool_calls: None,
            tool_call_id: Some("tc-001".into()),
            is_error: None,
        };
        log.append(msg).await.unwrap();

        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        assert_eq!(rebuilt.history().len(), 1);
        assert_eq!(rebuilt.history()[0].role, "tool");
        assert_eq!(rebuilt.history()[0].tool_call_id.as_deref(), Some("tc-001"));
    }

    #[tokio::test]
    async fn assistant_with_tool_calls_round_trips() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        let msg = Message {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc-001".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
            }]),
            tool_call_id: None,
            is_error: None,
        };
        log.append_assistant_tagged(msg, None, AssistantAttribution::default())
            .await
            .unwrap();

        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        let history = rebuilt.history();
        let tool_calls = history[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "bash");
    }

    #[tokio::test]
    async fn append_assistant_tagged_rejects_empty_message() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        let msg = Message {
            role: "assistant".into(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
        };
        let err = log
            .append_assistant_tagged(msg, None, AssistantAttribution::default())
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
    async fn truncate_rolls_back_history_and_deletes_event_files() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        log.append(text_msg("user", "First")).await.unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "Second"),
            None,
            AssistantAttribution::default(),
        )
        .await
        .unwrap();
        log.append(text_msg("user", "Third")).await.unwrap();
        assert_eq!(log.len(), 3);

        log.truncate(1).await;
        assert_eq!(log.len(), 1);
        assert_eq!(log.history()[0].role, "user");

        assert!(
            tmp.path().join("event-000001.json").exists(),
            "kept entry's file must remain"
        );
        assert!(
            !tmp.path().join("event-000002.json").exists(),
            "truncated entry's file must be deleted"
        );
        assert!(
            !tmp.path().join("event-000003.json").exists(),
            "truncated entry's file must be deleted"
        );

        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        assert_eq!(rebuilt.history().len(), 1);
    }

    #[tokio::test]
    async fn rebuild_fails_on_corrupted_event_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("event-000001.json"),
            "{\"ts\":\"t\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}",
        )
        .unwrap();
        std::fs::write(tmp.path().join("event-000002.json"), "not json").unwrap();
        assert!(
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .is_err(),
            "should fail on corrupted event file"
        );
    }

    #[tokio::test]
    async fn delegate_scope_isolates_per_call_and_orchestrator_excludes_them() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        // Orchestrator user input
        log.append(text_msg("user", "do thing")).await.unwrap();
        // Orchestrator assistant tool_use (untagged)
        log.append_assistant_tagged(
            text_msg("assistant", "calling tool"),
            None,
            AssistantAttribution::default(),
        )
        .await
        .unwrap();
        // Delegate call A
        log.append_tagged(
            text_msg("user", "delegate A query"),
            Some("delegate:call-A".into()),
        )
        .await
        .unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "delegate A reply"),
            Some("delegate:call-A".into()),
            AssistantAttribution::default(),
        )
        .await
        .unwrap();
        // Delegate call B
        log.append_tagged(
            text_msg("user", "delegate B query"),
            Some("delegate:call-B".into()),
        )
        .await
        .unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "delegate B reply"),
            Some("delegate:call-B".into()),
            AssistantAttribution::default(),
        )
        .await
        .unwrap();
        // Orchestrator final reply (untagged)
        log.append_assistant_tagged(
            text_msg("assistant", "final"),
            None,
            AssistantAttribution::default(),
        )
        .await
        .unwrap();

        let orch = log.history_for_provider(HistoryScope::Orchestrator);
        assert_eq!(
            orch.len(),
            3,
            "orchestrator scope excludes all delegate entries"
        );
        assert_eq!(content_text(&orch[0].content), Some("do thing"));
        assert_eq!(content_text(&orch[1].content), Some("calling tool"));
        assert_eq!(content_text(&orch[2].content), Some("final"));

        let delegate_a = log.history_for_provider(HistoryScope::Delegate("call-A"));
        assert_eq!(delegate_a.len(), 2);
        assert_eq!(
            content_text(&delegate_a[0].content),
            Some("delegate A query")
        );
        assert_eq!(
            content_text(&delegate_a[1].content),
            Some("delegate A reply")
        );

        let delegate_b = log.history_for_provider(HistoryScope::Delegate("call-B"));
        assert_eq!(delegate_b.len(), 2);
        assert_eq!(
            content_text(&delegate_b[0].content),
            Some("delegate B query")
        );
        assert_eq!(
            content_text(&delegate_b[1].content),
            Some("delegate B reply")
        );
    }

    #[tokio::test]
    async fn assistant_attribution_round_trips_through_rebuild() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log =
                ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
            log.append(text_msg("user", "hi")).await.unwrap();
            log.append_assistant_tagged(
                text_msg("assistant", "hello back"),
                None,
                AssistantAttribution {
                    model: Some("default".into()),
                    system_prompt_sha256: Some(sha256_hex("You are helpful.")),
                    warnings: vec![],
                },
            )
            .await
            .unwrap();
            log.append_assistant_tagged(
                text_msg("assistant", "delegate response"),
                Some("delegate:abc".into()),
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

        // Main-thread assistant: model + hash.
        assert_eq!(attrs[1].model.as_deref(), Some("default"));
        assert_eq!(
            attrs[1].system_prompt_sha256.as_deref(),
            Some(sha256_hex("You are helpful.").as_str())
        );

        // Delegate assistant: model + hash.
        assert_eq!(attrs[2].model.as_deref(), Some("anthropic.haiku"));
        assert_eq!(
            attrs[2].system_prompt_sha256.as_deref(),
            Some(sha256_hex("You are alice.").as_str())
        );
    }

    #[tokio::test]
    async fn sha256_hex_is_stable() {
        // Lowercase hex, 64 chars for SHA-256.
        let h = sha256_hex("abc");
        assert_eq!(h.len(), 64);
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[tokio::test]
    async fn frontmatter_passthrough_when_absent() {
        let input = "You are a helpful assistant.";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, input);
        assert!(fm.model.is_none());
    }

    #[tokio::test]
    async fn frontmatter_extracts_model_and_strips() {
        let input = "---\nmodel: smart\n---\nYou are Alice.";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "You are Alice.");
        assert_eq!(fm.model.as_deref(), Some("smart"));
    }

    #[tokio::test]
    async fn frontmatter_strips_even_when_model_missing() {
        let input = "---\nname: alice\ndescription: warm\n---\nYou are Alice.";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "You are Alice.");
        assert!(fm.model.is_none());
    }

    #[tokio::test]
    async fn frontmatter_handles_crlf() {
        let input = "---\r\nmodel: smart\r\n---\r\nYou are Alice.";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "You are Alice.");
        assert_eq!(fm.model.as_deref(), Some("smart"));
    }

    #[tokio::test]
    async fn frontmatter_strips_utf8_bom() {
        let input = "\u{FEFF}---\nmodel: smart\n---\nbody";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "body");
        assert_eq!(fm.model.as_deref(), Some("smart"));
    }

    #[tokio::test]
    async fn frontmatter_passthrough_when_missing_closer() {
        // No closing --- → not actually frontmatter; pass through.
        let input = "---\nmodel: smart\nYou are Alice.";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, input);
        assert!(fm.model.is_none());
    }

    #[tokio::test]
    async fn frontmatter_passthrough_when_yaml_invalid() {
        let input = "---\n: : not valid : yaml :\n---\nbody";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, input);
        assert!(fm.model.is_none());
    }

    #[tokio::test]
    async fn frontmatter_ignores_non_string_model_field() {
        // model is a list, not a string → ignored, but body still stripped
        // because the frontmatter delimiters parsed cleanly.
        let input = "---\nmodel:\n  - smart\n  - fast\n---\nbody";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "body");
        assert!(fm.model.is_none());
    }

    #[tokio::test]
    async fn frontmatter_passthrough_when_closer_past_scan_limit() {
        // Build a frontmatter whose closing --- sits past the 4 KiB cap.
        let mut input = String::from("---\nmodel: smart\n");
        // Pad with comment lines until we exceed 4 KiB before the closer.
        while input.len() < 5 * 1024 {
            input.push_str("# pad pad pad pad pad pad pad pad\n");
        }
        input.push_str("---\nbody");
        let (body, fm) = strip_frontmatter(&input);
        assert_eq!(
            body, input,
            "should pass through unchanged when closer is past 4 KiB"
        );
        assert!(fm.model.is_none());
    }

    #[tokio::test]
    async fn frontmatter_empty_body_is_permitted() {
        let input = "---\nmodel: smart\n---\n";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "");
        assert_eq!(fm.model.as_deref(), Some("smart"));
    }

    #[tokio::test]
    async fn frontmatter_extracts_params_block() {
        let input =
            "---\nparams:\n  output_config:\n    effort: high\n  max_tokens: 16000\n---\nbody";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "body");
        let params = fm.params.expect("params must be extracted");
        assert_eq!(
            params.get("output_config").and_then(|v| v.get("effort")),
            Some(&serde_json::Value::String("high".into()))
        );
        assert_eq!(
            params.get("max_tokens"),
            Some(&serde_json::Value::Number(16000.into()))
        );
    }

    #[tokio::test]
    async fn frontmatter_without_params_returns_none() {
        let input = "---\nmodel: smart\n---\nbody";
        let (_body, fm) = strip_frontmatter(input);
        assert!(fm.params.is_none());
    }

    #[tokio::test]
    async fn frontmatter_with_non_mapping_params_returns_none() {
        let input = "---\nparams: 42\n---\nbody";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "body");
        assert!(fm.params.is_none());
    }

    #[tokio::test]
    async fn last_assistant_model_returns_none_for_empty_log() {
        let tmp = TempDir::new().unwrap();
        let log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
        assert!(log
            .last_assistant_model(HistoryScope::Orchestrator)
            .is_none());
    }

    #[tokio::test]
    async fn last_assistant_model_returns_most_recent_assistant_in_orchestrator_scope() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        log.append(text_msg("user", "hi")).await.unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "older"),
            None,
            AssistantAttribution {
                model: Some("haiku".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
        .await
        .unwrap();
        log.append(text_msg("user", "again")).await.unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "newer"),
            None,
            AssistantAttribution {
                model: Some("sonnet".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
        .await
        .unwrap();

        assert_eq!(
            log.last_assistant_model(HistoryScope::Orchestrator)
                .as_deref(),
            Some("sonnet")
        );
    }

    #[tokio::test]
    async fn last_assistant_model_skips_user_and_tool_entries() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        log.append_assistant_tagged(
            text_msg("assistant", "earlier"),
            None,
            AssistantAttribution {
                model: Some("haiku".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
        .await
        .unwrap();
        log.append(text_msg("user", "more")).await.unwrap();
        log.append(text_msg("tool", "result")).await.unwrap();

        assert_eq!(
            log.last_assistant_model(HistoryScope::Orchestrator)
                .as_deref(),
            Some("haiku"),
            "user and tool entries must be skipped"
        );
    }

    #[tokio::test]
    async fn last_assistant_model_filters_by_delegate_scope() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        log.append_assistant_tagged(
            text_msg("assistant", "orchestrator"),
            None,
            AssistantAttribution {
                model: Some("orchestrator-model".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
        .await
        .unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "delegate alice"),
            Some("delegate:alice-1".into()),
            AssistantAttribution {
                model: Some("delegate-model".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
        .await
        .unwrap();

        assert_eq!(
            log.last_assistant_model(HistoryScope::Orchestrator)
                .as_deref(),
            Some("orchestrator-model"),
            "orchestrator scope must skip delegate entries"
        );
        assert_eq!(
            log.last_assistant_model(HistoryScope::Delegate("alice-1"))
                .as_deref(),
            Some("delegate-model"),
            "delegate scope must select that delegate's entry"
        );
    }

    #[tokio::test]
    async fn last_assistant_model_skips_assistants_without_model_attribution() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));

        log.append_assistant_tagged(
            text_msg("assistant", "with model"),
            None,
            AssistantAttribution {
                model: Some("haiku".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
        .await
        .unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "no model"),
            None,
            AssistantAttribution::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            log.last_assistant_model(HistoryScope::Orchestrator)
                .as_deref(),
            Some("haiku"),
            "entries without model attribution must be skipped"
        );
    }

    #[tokio::test]
    async fn assistant_attribution_warnings_round_trip() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log =
                ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
            log.append_assistant_tagged(
                text_msg("assistant", "ok"),
                None,
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
        // Event file without the `warnings` field; serde defaults to empty.
        std::fs::write(
            tmp.path().join("event-000001.json"),
            "{\"ts\":\"t\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}],\"model\":\"haiku\"}",
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
        // Catches `replace ConversationLog::is_empty -> bool with true`.
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())));
        assert!(log.is_empty(), "fresh log should be empty");

        log.append(text_msg("user", "hi")).await.unwrap();

        assert!(!log.is_empty(), "log with 1 entry must not report empty");
        assert_eq!(log.len(), 1);
    }

    #[tokio::test]
    async fn frontmatter_scan_limit_is_4kib() {
        // Catches `replace * with +` on `4 * 1024` — the limit must equal 4096
        // bytes. Build inputs with a closing `---` slightly before vs. after
        // the limit, and assert detection differs at the boundary.
        // 4*1024 = 4096; 4+1024 = 1028 (mutation).
        // A frontmatter ~3000 bytes long fits in 4096 but exceeds 1028, so
        // the original code finds the closer (returns "body"), the mutant
        // doesn't (returns the input unchanged).
        let mut input = String::from("---\n");
        while input.len() < 3000 {
            input.push_str("# pad-line\n");
        }
        input.push_str("---\nbody");
        let (body, _fm) = strip_frontmatter(&input);
        assert_eq!(
            body, "body",
            "closer at byte ~3000 must be found within the 4 KiB scan limit"
        );
    }

    #[tokio::test]
    async fn rebuild_handles_entries_without_tag() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("event-000001.json"),
            "{\"ts\":\"t\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Hi\"}]}",
        )
        .unwrap();

        let rebuilt =
            ConversationLog::rebuild(Arc::new(LocalFsStore::new(tmp.path().to_path_buf())))
                .await
                .unwrap();
        assert_eq!(rebuilt.history().len(), 1);
        assert_eq!(rebuilt.history()[0].role, "user");
        let history = rebuilt.history_for_provider(HistoryScope::Orchestrator);
        assert_eq!(
            history.len(),
            1,
            "untagged entry must remain visible in orchestrator scope"
        );
    }

    // --- snapshot() / ts preservation ---

    fn fresh_log() -> ConversationLog {
        let tmp = TempDir::new().unwrap().keep();
        ConversationLog::new(Arc::new(LocalFsStore::new(tmp)))
    }

    #[tokio::test]
    async fn snapshot_empty_log_returns_empty_with_zero_total() {
        let log = fresh_log();
        let snap = log.snapshot(None);
        assert!(snap.entries.is_empty());
        assert_eq!(snap.total_seq, 0);
    }

    #[tokio::test]
    async fn snapshot_none_returns_all_entries_in_order() {
        let mut log = fresh_log();
        log.append(text_msg("user", "first")).await.unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "second"),
            None,
            AssistantAttribution::default(),
        )
        .await
        .unwrap();
        log.append(text_msg("user", "third")).await.unwrap();

        let snap = log.snapshot(None);
        assert_eq!(snap.entries.len(), 3);
        assert_eq!(snap.total_seq, 3);
        assert_eq!(snap.entries[0].seq, 1);
        assert_eq!(snap.entries[1].seq, 2);
        assert_eq!(snap.entries[2].seq, 3);
        assert_eq!(
            content_text(&snap.entries[0].message.content),
            Some("first")
        );
        assert_eq!(
            content_text(&snap.entries[1].message.content),
            Some("second")
        );
        assert_eq!(
            content_text(&snap.entries[2].message.content),
            Some("third")
        );
    }

    #[tokio::test]
    async fn snapshot_limit_zero_returns_all_entries() {
        // Plan says: Some(0) → same as None (no limit). The proto wire
        // contract treats unset / 0 as "no limit"; conversation.rs
        // mirrors that.
        let mut log = fresh_log();
        for n in 0..5 {
            log.append(text_msg("user", &format!("msg-{n}")))
                .await
                .unwrap();
        }
        let snap = log.snapshot(Some(0));
        assert_eq!(snap.entries.len(), 5);
    }

    #[tokio::test]
    async fn snapshot_limit_returns_tail() {
        let mut log = fresh_log();
        for n in 0..5 {
            log.append(text_msg("user", &format!("msg-{n}")))
                .await
                .unwrap();
        }
        let snap = log.snapshot(Some(2));
        assert_eq!(snap.entries.len(), 2);
        assert_eq!(snap.total_seq, 5);
        // Tail: msg-3 (seq 4) + msg-4 (seq 5).
        assert_eq!(snap.entries[0].seq, 4);
        assert_eq!(snap.entries[1].seq, 5);
        assert_eq!(
            content_text(&snap.entries[0].message.content),
            Some("msg-3")
        );
        assert_eq!(
            content_text(&snap.entries[1].message.content),
            Some("msg-4")
        );
    }

    #[tokio::test]
    async fn snapshot_limit_exceeding_length_returns_all() {
        let mut log = fresh_log();
        log.append(text_msg("user", "only")).await.unwrap();
        let snap = log.snapshot(Some(100));
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.total_seq, 1);
    }

    #[tokio::test]
    async fn snapshot_preserves_tag() {
        let mut log = fresh_log();
        log.append_tagged(text_msg("user", "tagged"), Some("delegate:abc".into()))
            .await
            .unwrap();
        let snap = log.snapshot(None);
        assert_eq!(snap.entries[0].tag.as_deref(), Some("delegate:abc"));
    }

    #[tokio::test]
    async fn snapshot_ts_is_rfc3339_and_per_entry_stable() {
        let mut log = fresh_log();
        log.append(text_msg("user", "one")).await.unwrap();
        log.append(text_msg("user", "two")).await.unwrap();
        let snap_a = log.snapshot(None);
        let snap_b = log.snapshot(None);
        // ts must be RFC 3339 parseable.
        chrono::DateTime::parse_from_rfc3339(&snap_a.entries[0].ts).unwrap();
        // ts is captured at append-time and stable across reads (not
        // regenerated each snapshot call). Catches a regression to
        // `entry_to_log_entry`'s prior behavior of calling Utc::now()
        // at write-time, which would diverge ts on rebuild.
        assert_eq!(snap_a.entries[0].ts, snap_b.entries[0].ts);
        assert_eq!(snap_a.entries[1].ts, snap_b.entries[1].ts);
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
    async fn write_meta_roundtrip_preserves_name() {
        let tmp = TempDir::new().unwrap();
        let store = LocalFsStore::new(tmp.path().to_path_buf());
        store.write_meta("Quarterly review").await.unwrap();
        assert_eq!(
            store.read_meta().await.unwrap().as_deref(),
            Some("Quarterly review"),
        );
    }

    #[tokio::test]
    async fn write_meta_uses_tmp_rename_so_stale_tmp_does_not_survive() {
        // A previous controller crashed mid-write and left a meta.json.tmp
        // behind. The next successful write_meta must atomically promote
        // a fresh tmp to meta.json AND leave no leftover tmp on disk.
        // Mutation target: collapse the tmp+rename pair to a single
        // `fs::write(&final_path, json)` in LocalFsStore::write_meta —
        // the stale tmp from line below would survive, and the
        // `!meta.json.tmp.exists()` assertion below goes red.
        let tmp = TempDir::new().unwrap();
        let store = LocalFsStore::new(tmp.path().to_path_buf());
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(tmp.path().join(META_TMP_FILENAME), "stale-crash-debris").unwrap();
        // Sanity: pre-write state matches what we set up.
        assert!(tmp.path().join(META_TMP_FILENAME).exists());
        assert!(!tmp.path().join(META_FILENAME).exists());

        store.write_meta("Real Name").await.unwrap();

        assert_eq!(
            store.read_meta().await.unwrap().as_deref(),
            Some("Real Name"),
        );
        assert!(
            !tmp.path().join(META_TMP_FILENAME).exists(),
            "atomic rename must consume meta.json.tmp"
        );
    }

    #[tokio::test]
    async fn read_meta_returns_none_when_sidecar_missing() {
        let tmp = TempDir::new().unwrap();
        let store = LocalFsStore::new(tmp.path().to_path_buf());
        assert!(store.read_meta().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn walk_conversations_returns_id_and_name_per_meta_json() {
        let tmp = TempDir::new().unwrap();
        let factory = LocalFsFactory::new(tmp.path().to_path_buf());

        factory
            .make_store("ws", "alpha")
            .write_meta("Alpha chat")
            .await
            .unwrap();
        factory
            .make_store("ws", "beta")
            .write_meta("Beta chat")
            .await
            .unwrap();

        // A bare subdirectory without meta.json — must be skipped, not error.
        fs::create_dir_all(tmp.path().join("ws").join("gamma")).unwrap();
        // A stray file at the workspace root — must not be enumerated.
        fs::write(tmp.path().join("ws").join("readme.txt"), "noise").unwrap();

        let mut walked = factory.walk_conversations("ws").await.unwrap();
        walked.sort();
        assert_eq!(
            walked,
            vec![
                ("alpha".to_string(), "Alpha chat".to_string()),
                ("beta".to_string(), "Beta chat".to_string()),
            ],
        );
    }

    #[tokio::test]
    async fn walk_conversations_returns_empty_when_workspace_dir_missing() {
        // First boot: no conversations on disk yet. Walk must not error.
        let tmp = TempDir::new().unwrap();
        let factory = LocalFsFactory::new(tmp.path().to_path_buf());
        let walked = factory.walk_conversations("fresh-ws").await.unwrap();
        assert!(walked.is_empty());
    }
}
