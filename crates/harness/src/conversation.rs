use async_trait::async_trait;
use chrono::Utc;
use proto_common::{content_block, image_block, text_content, ContentBlock, Message, ToolCall};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;

/// The agent's current conversation window at the conversation root:
/// append-only ND-JSON, one JSON record per line in append order. Archives
/// sit beside it as `conversation.log.<n>.gz`, written by logrotate on
/// compact, 1 the newest.
pub(crate) const CONVERSATION_FILENAME: &str = "conversation.log";

/// The logrotate config the harness writes at boot, beside the log.
const LOGROTATE_CONF_FILENAME: &str = "conversation.logrotate";

/// The image's logrotate and gzip. logrotate's compiled-in compress command
/// is `/bin/gzip`.
const LOGROTATE: &str = "/bin/logrotate";
const GZIP: &str = "/bin/gzip";

/// The most archives an agent may keep; no archive is numbered above it.
pub(crate) const MAX_ARCHIVES: u32 = 1000;

/// The most uncompressed bytes one archive may hold when it is read.
const ARCHIVE_READ_CAP: u64 = 16 * 1024 * 1024;

/// The first line of the entry a compaction starts the new window with.
const SUMMARY_PREFIX: &str = "Summary of the earlier conversation:\n\n";

/// Storage backend for the agent's one conversation.
#[async_trait]
pub(crate) trait ConversationStore: Send + Sync {
    /// Append a single event as one JSON line to `conversation.log`, in
    /// arrival order. A write that fails part way is cut back, so the file
    /// never keeps a partial line.
    async fn append_event(&self, entry: &LogEntry) -> Result<(), String>;
    /// Read every event in `conversation.log`, in append order. A torn last
    /// line is cut at startup; any other unparsable line is skipped.
    async fn read_all(&self) -> Result<Vec<LogEntry>, String>;
    /// Empty `conversation.log` in place (same file, 0 bytes). A missing
    /// file stays missing. Archives are not touched.
    async fn clear(&self) -> Result<(), String>;
    /// Archive the current window: logrotate copies `conversation.log` into
    /// `conversation.log.1.gz` and keeps the newest N archives. Archives above
    /// N are then removed, then `conversation.log`. On Err,
    /// `conversation.log` is as it was.
    async fn archive(&self) -> Result<(), String>;
    /// Read the events of archive `conversation.log.<n>.gz`. `Ok(None)` when
    /// no such file exists.
    async fn read_archive(&self, n: u64) -> Result<Option<Vec<LogEntry>>, String>;
}

/// Local-filesystem store at the conversation root (a directory on the
/// per-agent conversation PVC): `conversation.log` plus its archives, and
/// `conversation.log.1`, the copy a failed compact leaves, which it removes.
/// Every other name at the root is never touched.
pub(crate) struct LocalFsStore {
    root: PathBuf,
    /// N: how many archives a compact keeps.
    archives: u32,
}

impl LocalFsStore {
    pub(crate) fn new(root: PathBuf, archives: u32) -> Self {
        Self { root, archives }
    }
}

/// Cut a last line with no closing newline (a write cut short by a crash)
/// off the ND-JSON file at `path`, keeping every whole line. A missing, empty
/// or newline-terminated file is left as it is.
pub(crate) async fn cut_torn_tail(path: PathBuf) -> Result<(), String> {
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
        };
        if bytes.is_empty() || bytes.ends_with(b"\n") {
            return Ok(());
        }
        let keep = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_len(keep as u64))
            .map_err(|e| {
                format!(
                    "failed to cut the torn last line of {}: {e}",
                    path.display()
                )
            })
    })
    .await
    .map_err(|e| format!("cut_torn_tail join error: {e}"))?
}

/// Parse ND-JSON text, skipping a blank line and warning on and skipping an
/// unparsable one. `what` names the file in the warning.
pub(crate) fn parse_ndjson<T: DeserializeOwned>(text: &str, what: &str) -> Vec<T> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(item) => out.push(item),
            Err(e) => tracing::warn!(error = %e, "skipping unparsable {what} line"),
        }
    }
    out
}

trait AppendTarget: io::Write {
    fn len(&self) -> io::Result<u64>;
    fn set_len(&mut self, len: u64) -> io::Result<()>;
}

impl AppendTarget for fs::File {
    fn len(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        fs::File::set_len(self, len)
    }
}

/// Write `line` whole, or cut the target back to its length before the
/// write and return the write error.
fn append_or_cut_back(target: &mut impl AppendTarget, line: &[u8]) -> Result<(), String> {
    let before = target
        .len()
        .map_err(|e| format!("failed to measure the log: {e}"))?;
    if let Err(e) = target.write_all(line) {
        return Err(match target.set_len(before) {
            Ok(()) => format!("failed to append event: {e}"),
            Err(cut) => format!(
                "failed to append event: {e}; and failed to cut the log back to {before} bytes: {cut}"
            ),
        });
    }
    Ok(())
}

/// `<logrotate> -f -s /dev/null <root>/conversation.logrotate`: forced, no
/// state file, the harness's own config. Run by fork and exec, no shell.
fn logrotate_command(logrotate: &Path, root: &Path) -> Command {
    let mut cmd = Command::new(logrotate);
    cmd.args(["-f", "-s", "/dev/null"])
        .arg(root.join(LOGROTATE_CONF_FILENAME));
    cmd
}

/// `<gzip> -dc -- <path>`: decompress an archive to stdout.
fn gzip_command(gzip: &Path, path: &Path) -> Command {
    let mut cmd = Command::new(gzip);
    cmd.args(["-dc", "--"]).arg(path);
    cmd
}

/// Write the logrotate config for `<root>/conversation.log`: one absolute
/// path, no glob, `rotate N`, `compress` and `copy` and nothing else.
/// `copy` leaves the log in place, so a failed gzip loses nothing. Overwrites an
/// older file whole.
pub(crate) fn write_logrotate_conf(root: &Path, n: u32) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = root.join(LOGROTATE_CONF_FILENAME);
    let text = format!(
        "{} {{\n    rotate {n}\n    compress\n    copy\n}}\n",
        root.join(CONVERSATION_FILENAME).display()
    );
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|e| format!("failed to write {}: {e}", path.display()))
}

/// Run `logrotate` over the root's config, confirm it wrote a new
/// `conversation.log.1.gz`, remove every archive above `n`, then remove
/// `conversation.log`. On any error `conversation.log` is as it was.
async fn archive_with(logrotate: &Path, root: &Path, n: u32) -> Result<(), String> {
    let newest = root.join(format!("{CONVERSATION_FILENAME}.1.gz"));
    let before = file_id(&newest).await;
    let output = tokio::process::Command::from(logrotate_command(logrotate, root))
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run {}: {e}", logrotate.display()))?;
    if !output.status.success() {
        let mut err = format!(
            "logrotate failed (exit {}): {}",
            exit_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        // A failed gzip leaves conversation.log.1, a copy of the intact log.
        let copy = root.join(format!("{CONVERSATION_FILENAME}.1"));
        match tokio::fs::remove_file(&copy).await {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => err.push_str(&format!("; and failed to remove {}: {e}", copy.display())),
        }
        return Err(err);
    }
    // The last compact's .1.gz is still there, so only a different file
    // shows a new archive.
    let after = file_id(&newest).await;
    if after.is_none() || after == before {
        return Err("logrotate exited 0 but did not rotate conversation.log".to_string());
    }
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        prune_archives(&root, n)?;
        let log = root.join(CONVERSATION_FILENAME);
        match fs::remove_file(&log) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("failed to remove {}: {e}", log.display())),
        }
    })
    .await
    .map_err(|e| format!("archive join error: {e}"))?
}

/// (dev, inode) of the regular file at `path`; `None` when there is none.
async fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    tokio::fs::metadata(path)
        .await
        .ok()
        .filter(|m| m.is_file())
        .map(|m| (m.dev(), m.ino()))
}

fn exit_code(status: ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| "none".to_string(), |c| c.to_string())
}

/// logrotate keeps archives above N after N was lowered, so this is what
/// holds the count at N.
fn prune_archives(root: &Path, n: u32) -> Result<(), String> {
    for k in n.saturating_add(1)..=MAX_ARCHIVES {
        let path = root.join(format!("{CONVERSATION_FILENAME}.{k}.gz"));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("failed to remove {}: {e}", path.display())),
        }
    }
    Ok(())
}

/// Decompress the archive at `path` with `gzip` and parse its entries. A
/// non-zero gzip exit is an error, never a short list, and output over
/// `ARCHIVE_READ_CAP` is refused without waiting on gzip.
async fn gunzip_entries(gzip: &Path, path: &Path) -> Result<Vec<LogEntry>, String> {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::from(gzip_command(gzip, path))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not be read (failed to run gzip: {e})"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "could not be read (no gzip output)".to_string())?;
    let mut bytes = Vec::new();
    stdout
        .take(ARCHIVE_READ_CAP + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| format!("could not be read ({e})"))?;
    if bytes.len() as u64 > ARCHIVE_READ_CAP {
        // `kill_on_drop` stops a gzip blocked on the full pipe.
        return Err(format!(
            "is larger than {} MiB uncompressed",
            ARCHIVE_READ_CAP >> 20
        ));
    }
    let status = child
        .wait()
        .await
        .map_err(|e| format!("could not be read ({e})"))?;
    if !status.success() {
        return Err(format!(
            "could not be read (gzip exit {})",
            exit_code(status)
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| "is not UTF-8".to_string())?;
    Ok(parse_log(&text))
}

fn parse_log(contents: &str) -> Vec<LogEntry> {
    parse_ndjson(contents, "conversation log")
}

/// Read and parse one log file; a missing file is empty.
fn read_log_file(path: &Path) -> Result<Vec<LogEntry>, String> {
    match fs::read_to_string(path) {
        Ok(c) => Ok(parse_log(&c)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("failed to read {}: {e}", path.display())),
    }
}

#[async_trait]
impl ConversationStore for LocalFsStore {
    async fn append_event(&self, entry: &LogEntry) -> Result<(), String> {
        let path = self.root.join(CONVERSATION_FILENAME);
        let mut line = serde_json::to_string(entry)
            .map_err(|e| format!("failed to serialize log entry: {e}"))?;
        line.push('\n');
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
            append_or_cut_back(&mut file, line.as_bytes())
                .map_err(|e| format!("{}: {e}", path.display()))
        })
        .await
        .map_err(|e| format!("append_event join error: {e}"))?
    }

    async fn read_all(&self) -> Result<Vec<LogEntry>, String> {
        let path = self.root.join(CONVERSATION_FILENAME);
        tokio::task::spawn_blocking(move || read_log_file(&path))
            .await
            .map_err(|e| format!("read_all join error: {e}"))?
    }

    async fn clear(&self) -> Result<(), String> {
        let path = self.root.join(CONVERSATION_FILENAME);
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            match fs::OpenOptions::new().write(true).open(&path) {
                Ok(file) => file
                    .set_len(0)
                    .map_err(|e| format!("failed to empty {}: {e}", path.display())),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("failed to open {}: {e}", path.display())),
            }
        })
        .await
        .map_err(|e| format!("clear join error: {e}"))?
    }

    async fn archive(&self) -> Result<(), String> {
        archive_with(Path::new(LOGROTATE), &self.root, self.archives).await
    }

    async fn read_archive(&self, n: u64) -> Result<Option<Vec<LogEntry>>, String> {
        let path = self.root.join(format!("{CONVERSATION_FILENAME}.{n}.gz"));
        match tokio::fs::metadata(&path).await {
            Ok(m) if m.is_file() => gunzip_entries(Path::new(GZIP), &path).await.map(Some),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("could not be read ({e})")),
        }
    }
}

/// Hex SHA-256 of `bytes`. Used to fingerprint the system prompt an LLM
/// ran under so audits can compare against the canonical instructions files
/// without storing the prompt verbatim on every entry.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LogEntry {
    pub(crate) ts: String,
    pub(crate) role: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) system_prompt_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AssistantAttribution {
    pub(crate) model: Option<String>,
    pub(crate) system_prompt_sha256: Option<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct Entry {
    /// RFC 3339 time of the append.
    ts: String,
    message: Message,
}

impl From<LogEntry> for Entry {
    fn from(log_entry: LogEntry) -> Self {
        Entry {
            ts: log_entry.ts,
            message: Message {
                role: log_entry.role,
                content: log_entry.content,
                tool_calls: log_entry.tool_calls,
                tool_call_id: log_entry.tool_call_id,
                is_error: log_entry.is_error,
            },
        }
    }
}

/// Rewrite one message as text only, for the no-tools compaction call and
/// for ReadArchive: an assistant's tool calls become text lines after its
/// own content, and a tool result becomes a user message holding its text.
pub(crate) fn tool_turn_as_text(mut m: Message) -> Message {
    for call in std::mem::take(&mut m.tool_calls) {
        m.content.push(proto_common::text_block(format!(
            "[tool call {}: {}]",
            call.name, call.input_json
        )));
    }
    if m.role == "tool" {
        let label = if m.is_error == Some(true) {
            "tool error"
        } else {
            "tool result"
        };
        m.role = "user".into();
        m.content = text_content(&format!(
            "[{label}]\n{}",
            proto_common::content_text(&m.content)
        ));
        m.tool_call_id = None;
        m.is_error = None;
    }
    m
}

/// Holds the proto `Message` directly — the harness gRPC handler passes it
/// straight onto the wire without conversion.
#[derive(Debug, Clone)]
pub(crate) struct EntrySnapshot {
    /// 1-indexed sequence in the log file.
    pub(crate) seq: u64,
    /// RFC 3339 timestamp from the original append.
    pub(crate) ts: String,
    pub(crate) message: Message,
}

impl EntrySnapshot {
    fn at(idx: usize, ts: String, message: Message) -> Self {
        Self {
            seq: (idx + 1) as u64,
            ts,
            message,
        }
    }
}

/// The agent's one conversation: the in-memory mirror of the current
/// `conversation.log` window plus its store.
pub(crate) struct ConversationLog {
    entries: Vec<Entry>,
    store: Arc<dyn ConversationStore>,
}

impl ConversationLog {
    #[cfg(test)]
    pub(crate) fn new(store: Arc<dyn ConversationStore>) -> Self {
        Self {
            entries: Vec::new(),
            store,
        }
    }

    /// Replay the current window (`conversation.log` only, never an
    /// archive) from `store` into a new in-memory log.
    pub(crate) async fn rebuild(store: Arc<dyn ConversationStore>) -> Result<Self, String> {
        let persisted = store.read_all().await?;
        let entries = persisted.into_iter().map(Entry::from).collect();
        Ok(Self { entries, store })
    }

    pub(crate) async fn append(&mut self, message: Message) -> Result<(), String> {
        if message.role == "assistant" {
            return Err("append rejects role=\"assistant\"; use append_assistant".to_string());
        }
        self.push(message, AssistantAttribution::default()).await
    }

    /// Returns Err if the message has no content blocks and no tool calls —
    /// persisting an empty assistant entry poisons the conversation log
    /// (subsequent turns fail with API 400).
    pub(crate) async fn append_assistant(
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
        self.push(message, attribution).await
    }

    async fn push(
        &mut self,
        message: Message,
        attribution: AssistantAttribution,
    ) -> Result<(), String> {
        let entry = Entry {
            ts: Utc::now().to_rfc3339(),
            message,
        };
        let log_entry = Self::entry_to_log_entry(&entry, attribution);
        self.store.append_event(&log_entry).await?;
        self.entries.push(entry);
        Ok(())
    }

    /// On a failed truncate the window is left as it was, so memory and disk
    /// agree.
    pub(crate) async fn clear(&mut self) -> Result<(), String> {
        self.store.clear().await?;
        self.entries.clear();
        Ok(())
    }

    /// Archive the window, then start the new one with `summary` alone. A
    /// failed archive leaves the window as it was; a summary that cannot be
    /// appended after a good archive leaves it empty.
    pub(crate) async fn compact(&mut self, summary: &str) -> Result<(), String> {
        self.store.archive().await?;
        self.entries.clear();
        self.append(Message {
            role: "user".into(),
            content: text_content(&format!("{SUMMARY_PREFIX}{summary}")),
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        })
        .await
    }

    pub(crate) async fn read_archive(&self, n: u64) -> Result<Option<Vec<EntrySnapshot>>, String> {
        Ok(self.store.read_archive(n).await?.map(|entries| {
            entries
                .into_iter()
                .map(Entry::from)
                .enumerate()
                .map(|(idx, e)| EntrySnapshot::at(idx, e.ts, e.message))
                .collect()
        }))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn history_for_provider(&self) -> Vec<Message> {
        self.entries.iter().map(|e| e.message.clone()).collect()
    }

    fn entry_to_log_entry(entry: &Entry, attribution: AssistantAttribution) -> LogEntry {
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
                    // bytes, so it persists unchanged, as does text/thinking.
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
            model: attribution.model,
            system_prompt_sha256: attribution.system_prompt_sha256,
            warnings: attribution.warnings,
        }
    }

    /// Up to `limit` newest entries of the window, oldest first; `None` or `Some(0)` returns all.
    pub(crate) fn snapshot(&self, limit: Option<usize>) -> ConversationSnapshot {
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
            .map(|(idx, e)| EntrySnapshot::at(idx, e.ts.clone(), e.message.clone()))
            .collect();
        ConversationSnapshot {
            entries,
            total_seq: total as u64,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ConversationSnapshot {
    pub(crate) entries: Vec<EntrySnapshot>,
    pub(crate) total_seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_doubles::{record, throwaway_log};
    use proto_common::{content_text, text_block};
    use tempfile::TempDir;

    fn store_at(root: &Path) -> Arc<LocalFsStore> {
        Arc::new(LocalFsStore::new(root.to_path_buf(), 10))
    }

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
        let mut log = ConversationLog::new(store_at(tmp.path()));
        let err = log
            .append(text_msg("assistant", "forged"))
            .await
            .expect_err("append must reject caller-supplied assistant role");
        assert!(
            err.contains("append rejects role=\"assistant\""),
            "error message should name the rejected role, got: {err}"
        );
        assert!(
            log.history_for_provider().is_empty(),
            "rejected append must not mutate history"
        );
    }

    #[tokio::test]
    async fn append_writes_one_ndjson_record_per_object_to_conversation_log() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(store_at(tmp.path()));

        log.append(text_msg("user", "Hello")).await.unwrap();
        log.append_assistant(
            text_msg("assistant", "Hi there"),
            AssistantAttribution::default(),
        )
        .await
        .unwrap();

        assert_eq!(log.history_for_provider().len(), 2);
        assert_eq!(log.history_for_provider()[0].role, "user");
        assert_eq!(log.history_for_provider()[1].role, "assistant");

        let conv = tmp.path().join("conversation.log");
        assert!(
            conv.is_file(),
            "the conversation log is a single conversation.log"
        );
        let text = fs::read_to_string(&conv).unwrap();
        let lines: Vec<&str> = text.lines().collect();
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
            let mut log = ConversationLog::new(store_at(tmp.path()));
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

        let rebuilt = ConversationLog::rebuild(store_at(tmp.path()))
            .await
            .unwrap();
        let history = rebuilt.history_for_provider();
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
        let rebuilt = ConversationLog::rebuild(store_at(tmp.path()))
            .await
            .unwrap();
        assert!(rebuilt.history_for_provider().is_empty());
    }

    #[tokio::test]
    async fn append_assistant_rejects_empty_message() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(store_at(tmp.path()));

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
            log.history_for_provider().is_empty(),
            "rejected append must not mutate history"
        );
    }

    #[tokio::test]
    async fn rebuild_skips_an_unparsable_line() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("conversation.log"),
            format!(
                "{}{{\"ts\":\"t\",\"role\":\"user\",\"conte\n{}",
                record("user", "before"),
                record("user", "after")
            ),
        )
        .unwrap();
        let rebuilt = ConversationLog::rebuild(store_at(tmp.path()))
            .await
            .expect("an unparsable line must not fail the rebuild");
        assert_eq!(
            texts_of(rebuilt.history_for_provider()),
            ["before", "after"],
            "the intact records still rebuild"
        );
    }

    /// The entries as written to `conversation.log` under `root`.
    fn logged_entries(root: &Path) -> Vec<LogEntry> {
        read_log_file(&root.join(CONVERSATION_FILENAME)).unwrap()
    }

    #[tokio::test]
    async fn assistant_attribution_is_written_with_each_entry() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log = ConversationLog::new(store_at(tmp.path()));
            log.append(text_msg("user", "hi")).await.unwrap();
            log.append_assistant(
                text_msg("assistant", "hello back"),
                AssistantAttribution {
                    model: Some("default".into()),
                    system_prompt_sha256: Some(sha256_hex(b"You are helpful.")),
                    warnings: vec![],
                },
            )
            .await
            .unwrap();
            log.append_assistant(
                text_msg("assistant", "second reply"),
                AssistantAttribution {
                    model: Some("anthropic.haiku".into()),
                    system_prompt_sha256: Some(sha256_hex(b"You are alice.")),
                    warnings: vec![],
                },
            )
            .await
            .unwrap();
        }

        let attrs = logged_entries(tmp.path());
        assert_eq!(attrs.len(), 3);

        assert!(attrs[0].model.is_none());
        assert!(attrs[0].system_prompt_sha256.is_none());

        assert_eq!(attrs[1].model.as_deref(), Some("default"));
        assert_eq!(
            attrs[1].system_prompt_sha256.as_deref(),
            Some(sha256_hex(b"You are helpful.").as_str())
        );

        assert_eq!(attrs[2].model.as_deref(), Some("anthropic.haiku"));
        assert_eq!(
            attrs[2].system_prompt_sha256.as_deref(),
            Some(sha256_hex(b"You are alice.").as_str())
        );
    }

    #[tokio::test]
    async fn assistant_attribution_warnings_are_written() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log = ConversationLog::new(store_at(tmp.path()));
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
        let attrs = logged_entries(tmp.path());
        assert_eq!(
            attrs[0].warnings,
            vec!["model".to_string(), "messages".to_string()]
        );
    }

    #[test]
    fn assistant_attribution_warnings_default_empty_when_omitted() {
        let entry: LogEntry = serde_json::from_str(
            "{\"ts\":\"t\",\"role\":\"assistant\",\"content\":[{\"block\":{\"Text\":{\"text\":\"hi\"}}}],\"model\":\"haiku\"}",
        )
        .unwrap();
        assert!(entry.warnings.is_empty());
    }

    #[tokio::test]
    async fn is_empty_reflects_actual_entry_count() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(store_at(tmp.path()));
        assert!(log.is_empty(), "fresh log should be empty");

        log.append(text_msg("user", "hi")).await.unwrap();

        assert!(!log.is_empty(), "log with 1 entry must not report empty");
        assert_eq!(log.history_for_provider().len(), 1);
    }

    #[tokio::test]
    async fn snapshot_limit_takes_the_tail_and_unset_zero_or_oversize_take_all() {
        let mut log = throwaway_log();
        log.append(text_msg("user", "msg-0")).await.unwrap();
        for n in 1..5 {
            log.append(text_msg("user", &format!("msg-{n}")))
                .await
                .unwrap();
        }

        let snap = log.snapshot(None);
        assert_eq!(snap.entries.len(), 5);
        assert_eq!(snap.total_seq, 5);
        for (i, entry) in snap.entries.iter().enumerate() {
            assert_eq!(entry.seq, i as u64 + 1);
            assert_eq!(content_text(&entry.message.content), format!("msg-{i}"));
        }

        assert_eq!(log.snapshot(Some(0)).entries.len(), 5);

        let snap = log.snapshot(Some(2));
        assert_eq!(snap.entries.len(), 2);
        assert_eq!(snap.total_seq, 5);
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
        let tmp = TempDir::new().unwrap();
        let store: Arc<dyn ConversationStore> = store_at(tmp.path());
        let mut log = ConversationLog::new(store.clone());
        log.append(text_msg("user", "persisted")).await.unwrap();
        let original_ts = log.snapshot(None).entries[0].ts.clone();

        drop(log);
        let rebuilt = ConversationLog::rebuild(store).await.unwrap();
        let rebuilt_ts = rebuilt.snapshot(None).entries[0].ts.clone();
        assert_eq!(rebuilt_ts, original_ts, "ts must survive rebuild");
    }

    // A tool-role image is persisted with its bytes emptied and its caption
    // and tool_call_id kept.
    #[tokio::test]
    async fn tool_role_image_bytes_are_stripped_from_conversation_history() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(store_at(tmp.path()));

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

        let conv = tmp.path().join("conversation.log");
        let text = fs::read_to_string(&conv).expect("conversation.log exists");
        assert!(
            !text.contains(marker),
            "a produced artifact's bytes must not be inlined into conversation.log, got {text:?}"
        );
        assert!(
            text.contains("rendered chart caption"),
            "the sibling caption text survives the strip, got {text:?}"
        );
        assert!(
            text.contains("tc-render-1"),
            "the tool_call_id survives the strip, got {text:?}"
        );

        let rebuilt = ConversationLog::rebuild(store_at(tmp.path()))
            .await
            .unwrap();
        let msg = rebuilt
            .history_for_provider()
            .into_iter()
            .next()
            .expect("one entry");
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
        let mut log = ConversationLog::new(store_at(tmp.path()));

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

        let conv = tmp.path().join("conversation.log");
        let text = fs::read_to_string(&conv).expect("conversation.log exists");
        assert!(
            text.contains(marker),
            "a user-supplied input image stays byte-inline in conversation.log, got {text:?}"
        );
    }

    // Every file and folder name under root, relative to it, sorted.
    fn tree_names(root: &Path) -> Vec<PathBuf> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                out.push(path.strip_prefix(root).unwrap().to_path_buf());
                if path.is_dir() {
                    walk(root, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    // A clear empties conversation.log in place: the same file (same inode)
    // is cut to 0 bytes, and no name at the root or in its folders is added
    // or removed. The archives, the execution log and a subdirectory's
    // conversation.log stay byte-equal. The next context, in memory and after
    // a restart, holds only what came after the clear.
    #[tokio::test]
    async fn clear_empties_conversation_log_in_place_and_leaves_archives_and_subdirectories() {
        use std::os::unix::fs::MetadataExt;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let archive_one = root.join("conversation.log.1.gz");
        let archive_two = root.join("conversation.log.2.gz");
        fs::write(&archive_one, b"archive-one-bytes").unwrap();
        fs::write(&archive_two, b"archive-two-bytes").unwrap();
        let execution = root.join("execution.json");
        fs::write(&execution, b"{\"call_id\":\"c1\"}\n").unwrap();
        let sub_log = root
            .join("research")
            .join("a3f9c2e1")
            .join("conversation.log");
        fs::create_dir_all(sub_log.parent().unwrap()).unwrap();
        fs::write(&sub_log, b"subdirectory-log\n").unwrap();

        let mut log = ConversationLog::rebuild(store_at(root)).await.unwrap();
        log.append(text_msg("user", "before the clear"))
            .await
            .unwrap();
        let current = root.join("conversation.log");
        let inode = fs::metadata(&current)
            .expect("the current window is conversation.log")
            .ino();
        let names = tree_names(root);

        log.clear().await.expect("clear empties the log");

        let after = fs::metadata(&current).expect("conversation.log still exists");
        assert_eq!(
            after.ino(),
            inode,
            "clear keeps the same conversation.log file"
        );
        assert_eq!(after.len(), 0, "clear empties conversation.log");
        assert_eq!(
            tree_names(root),
            names,
            "clear adds and removes no name at the root or in its folders"
        );
        assert_eq!(fs::read(&archive_one).unwrap(), b"archive-one-bytes");
        assert_eq!(fs::read(&archive_two).unwrap(), b"archive-two-bytes");
        assert_eq!(fs::read(&execution).unwrap(), b"{\"call_id\":\"c1\"}\n");
        assert_eq!(
            fs::read(&sub_log).unwrap(),
            b"subdirectory-log\n",
            "a subdirectory is left unchanged"
        );

        log.append(text_msg("user", "after the clear"))
            .await
            .unwrap();
        assert_eq!(
            texts_of(log.history_for_provider()),
            ["after the clear"],
            "the next context holds only what came after the clear"
        );
        let rebuilt = ConversationLog::rebuild(store_at(root)).await.unwrap();
        assert_eq!(
            texts_of(rebuilt.history_for_provider()),
            ["after the clear"],
            "a restart builds the context from conversation.log alone"
        );
    }

    // A file that takes the first `budget` bytes of a write and then fails
    // with ENOSPC, as a full disk does part way through a line.
    struct ShortFile {
        file: fs::File,
        budget: usize,
    }

    impl io::Write for ShortFile {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.budget == 0 {
                return Err(io::Error::from_raw_os_error(28));
            }
            let n = buf.len().min(self.budget);
            let written = self.file.write(&buf[..n])?;
            self.budget -= written;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }

    impl AppendTarget for ShortFile {
        fn len(&self) -> io::Result<u64> {
            Ok(self.file.metadata()?.len())
        }

        fn set_len(&mut self, len: u64) -> io::Result<()> {
            self.file.set_len(len)
        }
    }

    // A write that fails part way through a line is cut back: the call
    // returns an error and conversation.log is byte-equal to before, so no
    // torn line is left behind and the next append is a whole, parseable
    // line after the old ones.
    #[tokio::test]
    async fn a_failed_append_cuts_the_file_back_to_its_length_before_the_write() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conversation.log");
        let before = record("user", "already here");
        fs::write(&path, &before).unwrap();

        let file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        let mut short = ShortFile { file, budget: 5 };
        let line = record("user", "lost to a full disk");
        let result = append_or_cut_back(&mut short, line.as_bytes());
        drop(short);

        assert!(result.is_err(), "a short write is an error");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            before,
            "the file is cut back to its length before the write"
        );

        let mut log = ConversationLog::new(store_at(tmp.path()));
        log.append(text_msg("user", "next")).await.unwrap();
        let text = fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            serde_json::from_str::<LogEntry>(line)
                .unwrap_or_else(|e| panic!("every line parses, {line:?}: {e}"));
        }
        let texts: Vec<String> = logged_entries(tmp.path())
            .iter()
            .map(|e| content_text(&e.content))
            .collect();
        assert_eq!(texts, ["already here", "next"]);
    }

    // What the test store's `archive` does in place of logrotate.
    #[derive(Clone, Copy, Debug)]
    enum ArchiveStep {
        // conversation.log becomes conversation.log.1.gz; Ok.
        RenameOk,
        // Nothing on disk changes; Err.
        ErrUntouched,
    }

    // A store on a real LocalFsStore that stands in for logrotate in
    // `archive` and can refuse every append.
    struct ArchiveStub {
        inner: LocalFsStore,
        step: ArchiveStep,
        fail_append: bool,
    }

    impl ArchiveStub {
        fn new(root: &Path, step: ArchiveStep) -> Self {
            Self {
                inner: LocalFsStore::new(root.to_path_buf(), 10),
                step,
                fail_append: false,
            }
        }
    }

    #[async_trait]
    impl ConversationStore for ArchiveStub {
        async fn append_event(&self, entry: &LogEntry) -> Result<(), String> {
            if self.fail_append {
                return Err("append refused by the test store".into());
            }
            self.inner.append_event(entry).await
        }

        async fn read_all(&self) -> Result<Vec<LogEntry>, String> {
            self.inner.read_all().await
        }

        async fn clear(&self) -> Result<(), String> {
            self.inner.clear().await
        }

        async fn archive(&self) -> Result<(), String> {
            let rename = || {
                fs::rename(
                    self.inner.root.join("conversation.log"),
                    self.inner.root.join("conversation.log.1.gz"),
                )
                .unwrap()
            };
            match self.step {
                ArchiveStep::RenameOk => {
                    rename();
                    Ok(())
                }
                ArchiveStep::ErrUntouched => Err("archive failed".into()),
            }
        }

        async fn read_archive(&self, n: u64) -> Result<Option<Vec<LogEntry>>, String> {
            self.inner.read_archive(n).await
        }
    }

    fn texts_of(history: Vec<Message>) -> Vec<String> {
        history.iter().map(|m| content_text(&m.content)).collect()
    }

    // A compact archives the old window first, then starts the new one with
    // the summary as its only entry: role user, one text block with the
    // summary prefix, both on disk and in memory. The archive holds the old
    // window.
    #[tokio::test]
    async fn compact_archives_then_starts_the_window_with_the_summary_alone() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut log = ConversationLog::new(Arc::new(ArchiveStub::new(root, ArchiveStep::RenameOk)));
        log.append(text_msg("user", "remember the blue door"))
            .await
            .unwrap();
        log.append_assistant(
            text_msg("assistant", "noted"),
            AssistantAttribution::default(),
        )
        .await
        .unwrap();
        let old_window = fs::read(root.join("conversation.log")).unwrap();

        log.compact("the door is blue")
            .await
            .expect("compact succeeds");

        let want = "Summary of the earlier conversation:\n\nthe door is blue";
        assert_eq!(
            fs::read(root.join("conversation.log.1.gz")).unwrap(),
            old_window,
            "the archive holds the old window"
        );
        let text = fs::read_to_string(root.join("conversation.log")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "conversation.log holds one line, got {text:?}"
        );
        let entry: LogEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(entry.role, "user");
        assert_eq!(entry.content.len(), 1, "one text block");
        assert_eq!(content_text(&entry.content), want);

        let history = log.history_for_provider();
        assert_eq!(history.len(), 1, "the window holds the summary alone");
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content.len(), 1, "one text block");
        assert_eq!(content_text(&history[0].content), want);
    }

    // A failed compact returns its error. An archive that fails leaves the
    // old window; a summary that cannot be appended after a good archive
    // leaves an empty window.
    #[tokio::test]
    async fn a_failed_archive_keeps_the_window_and_a_failed_summary_empties_it() {
        for (step, fail_append, want_err, want_window) in [
            (
                ArchiveStep::ErrUntouched,
                false,
                "archive failed",
                vec!["a", "b"],
            ),
            (
                ArchiveStep::RenameOk,
                true,
                "append refused by the test store",
                vec![],
            ),
        ] {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            fs::write(
                root.join("conversation.log"),
                format!("{}{}", record("user", "a"), record("assistant", "b")),
            )
            .unwrap();
            let mut stub = ArchiveStub::new(root, step);
            stub.fail_append = fail_append;
            let mut log = ConversationLog::rebuild(Arc::new(stub)).await.unwrap();
            assert_eq!(texts_of(log.history_for_provider()), ["a", "b"]);

            let err = log
                .compact("the summary")
                .await
                .expect_err("a failed compact returns its error");
            assert_eq!(
                err, want_err,
                "archive {step:?}, summary append fails: {fail_append}"
            );

            assert_eq!(
                texts_of(log.history_for_provider()),
                want_window,
                "the window after a failed compact \
                 (archive {step:?}, summary append fails: {fail_append})"
            );
        }
    }

    #[test]
    fn archive_and_gzip_commands_are_fixed_argv_without_a_shell() {
        let root = Path::new("/var/lib/harness/conversations");
        let logrotate = logrotate_command(Path::new("/bin/logrotate"), root);
        let args: Vec<&std::ffi::OsStr> = logrotate.get_args().collect();
        assert_eq!(
            args,
            [
                "-f",
                "-s",
                "/dev/null",
                "/var/lib/harness/conversations/conversation.logrotate"
            ]
        );

        let archive = root.join("conversation.log.2.gz");
        let gzip = gzip_command(Path::new("/bin/gzip"), &archive);
        let args: Vec<&std::ffi::OsStr> = gzip.get_args().collect();
        assert_eq!(args, ["-dc", "--", archive.as_os_str().to_str().unwrap()]);
    }

    fn gzip_bytes(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut child = Command::new("/usr/bin/gzip")
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the host has /usr/bin/gzip");
        let mut stdin = child.stdin.take().unwrap();
        let input = bytes.to_vec();
        let writer = std::thread::spawn(move || stdin.write_all(&input).unwrap());
        let out = child.wait_with_output().unwrap();
        writer.join().unwrap();
        assert!(out.status.success());
        out.stdout
    }

    // An archive is read by running gzip: its entries come back oldest
    // first. A cut or non-gzip file is a read error carrying gzip's exit
    // code, never a short list, and an archive over 16 MiB uncompressed is
    // refused.
    #[tokio::test]
    async fn gunzip_entries_returns_entries_oldest_first_and_errors_on_damage() {
        let gzip = Path::new("/usr/bin/gzip");
        let tmp = TempDir::new().unwrap();
        let archive = tmp.path().join("conversation.log.1.gz");
        let packed = gzip_bytes(
            format!(
                "{}{}",
                record("user", "older"),
                record("assistant", "newer")
            )
            .as_bytes(),
        );
        fs::write(&archive, &packed).unwrap();

        let entries = gunzip_entries(gzip, &archive).await.unwrap();
        let texts: Vec<String> = entries.iter().map(|e| content_text(&e.content)).collect();
        assert_eq!(texts, ["older", "newer"]);
        let roles: Vec<&str> = entries.iter().map(|e| e.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant"]);

        let cut = tmp.path().join("conversation.log.2.gz");
        fs::write(&cut, &packed[..20]).unwrap();
        assert_eq!(
            gunzip_entries(gzip, &cut).await.err().as_deref(),
            Some("could not be read (gzip exit 1)"),
            "a cut archive"
        );

        let not_gzip = tmp.path().join("conversation.log.3.gz");
        fs::write(&not_gzip, record("user", "plain text, not gzip")).unwrap();
        assert_eq!(
            gunzip_entries(gzip, &not_gzip).await.err().as_deref(),
            Some("could not be read (gzip exit 1)"),
            "a file that is not gzip"
        );

        // Exactly 16 MiB + 1 bytes of whole NDJSON lines.
        let cap = ARCHIVE_READ_CAP as usize;
        let overhead = record("user", "").len();
        let mut big = String::with_capacity(cap + 1);
        while cap + 1 - big.len() >= 2 * (overhead + 1000) {
            big.push_str(&record("user", &"x".repeat(1000)));
        }
        let last = cap + 1 - big.len() - overhead;
        big.push_str(&record("user", &"y".repeat(last)));
        assert_eq!(big.len(), cap + 1);
        let large = tmp.path().join("conversation.log.4.gz");
        fs::write(&large, gzip_bytes(big.as_bytes())).unwrap();
        assert_eq!(
            gunzip_entries(gzip, &large).await.err().as_deref(),
            Some("is larger than 16 MiB uncompressed"),
            "an archive over the cap"
        );
    }

    // The boot-time logrotate config names one absolute path, no glob, with
    // `rotate N`, `compress` and `copy` and nothing else. It is created mode 0644 and
    // a reboot overwrites an older file whole.
    #[test]
    fn write_logrotate_conf_writes_one_absolute_path_with_rotate_n() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let conf = root.join("conversation.logrotate");
        let want = format!(
            "{} {{\n    rotate 3\n    compress\n    copy\n}}\n",
            root.join("conversation.log").display()
        );

        write_logrotate_conf(root, 3).unwrap();
        assert_eq!(fs::read_to_string(&conf).unwrap(), want);
        assert_eq!(
            fs::metadata(&conf).unwrap().permissions().mode() & 0o777,
            0o644,
            "the config is created mode 0644"
        );

        fs::write(
            &conf,
            "/elsewhere/* {\n    rotate 0\n    compresscmd /bin/sh\n    missingok\n}\n# older text\n",
        )
        .unwrap();
        write_logrotate_conf(root, 3).unwrap();
        assert_eq!(
            fs::read_to_string(&conf).unwrap(),
            want,
            "an older config is overwritten whole"
        );
    }

    // Every file and folder under root with its bytes (None for a folder).
    fn tree_bytes(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        tree_names(root)
            .into_iter()
            .map(|rel| {
                let path = root.join(&rel);
                let bytes = path.is_file().then(|| fs::read(&path).unwrap());
                (rel, bytes)
            })
            .collect()
    }

    // conversation.log (one line), archives 1 to 5 with known bytes, and a
    // subdirectory's log.
    fn seed_archive_root(root: &Path) {
        fs::write(root.join("conversation.log"), record("user", "the window")).unwrap();
        for k in 1..=5 {
            fs::write(
                root.join(format!("conversation.log.{k}.gz")),
                format!("archive-{k}-bytes"),
            )
            .unwrap();
        }
        let sub = root.join("research").join("r1");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("conversation.log"), b"subdirectory-log\n").unwrap();
    }

    fn stub_program(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    // A stub just written can be briefly busy while another test forks;
    // retry that only.
    async fn archive_with_retrying_busy(program: &Path, root: &Path, n: u32) -> Result<(), String> {
        let mut result = archive_with(program, root, n).await;
        for _ in 0..5 {
            match &result {
                Err(e) if e.contains("Text file busy") => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    result = archive_with(program, root, n).await;
                }
                _ => break,
            }
        }
        result
    }

    // `archive_with` runs the program it is given, then confirms the
    // rotation, then prunes. A rotation that writes a new
    // conversation.log.1.gz is Ok. It leaves only archives 1..=N and the
    // subdirectory, and no conversation.log. Exit 0 with no new `.1.gz`, or a
    // non-zero exit, is an error and leaves conversation.log as it was.
    #[tokio::test]
    async fn archive_runs_logrotate_then_confirms_then_prunes() {
        let programs = TempDir::new().unwrap();

        // (i) rotates, exit 0, N = 2.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_archive_root(root);
        let window = fs::read(root.join("conversation.log")).unwrap();
        let rotates = stub_program(
            programs.path(),
            "rotates",
            &format!(
                "cd '{0}' && for k in 5 4 3 2 1; do mv conversation.log.$k.gz conversation.log.$((k+1)).gz; done && cp conversation.log conversation.log.1.gz",
                root.display()
            ),
        );
        archive_with_retrying_busy(&rotates, root, 2)
            .await
            .expect("a confirmed rotation is Ok");
        assert_eq!(
            tree_bytes(root),
            vec![
                (PathBuf::from("conversation.log.1.gz"), Some(window)),
                (
                    PathBuf::from("conversation.log.2.gz"),
                    Some(b"archive-1-bytes".to_vec())
                ),
                (PathBuf::from("research"), None),
                (PathBuf::from("research/r1"), None),
                (
                    PathBuf::from("research/r1/conversation.log"),
                    Some(b"subdirectory-log\n".to_vec())
                ),
            ],
            "only archives 1..=2 and the subdirectory remain"
        );

        // (ii) exit 0 without rotating.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_archive_root(root);
        let seeded = tree_bytes(root);
        let does_nothing = stub_program(programs.path(), "does-nothing", "exit 0");
        assert_eq!(
            archive_with_retrying_busy(&does_nothing, root, 2).await,
            Err("logrotate exited 0 but did not rotate conversation.log".to_string())
        );
        assert_eq!(tree_bytes(root), seeded, "nothing is deleted or changed");

        // (iii) exit 1 with stderr.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_archive_root(root);
        let seeded = tree_bytes(root);
        let fails = stub_program(programs.path(), "fails", "echo boom >&2\nexit 1");
        let err = archive_with_retrying_busy(&fails, root, 2)
            .await
            .expect_err("a non-zero exit is an error");
        assert!(err.starts_with("logrotate failed (exit 1)"), "got {err:?}");
        assert_eq!(tree_bytes(root), seeded, "nothing is deleted or changed");

        // (iv) exit 0, copies conversation.log to .1 but writes no .1.gz.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let window = record("user", "the window");
        fs::write(root.join("conversation.log"), &window).unwrap();
        fs::write(root.join("conversation.log.3.gz"), "archive-3-bytes").unwrap();
        let copies = stub_program(
            programs.path(),
            "copies",
            &format!(
                "cp '{0}/conversation.log' '{0}/conversation.log.1'",
                root.display()
            ),
        );
        assert_eq!(
            archive_with_retrying_busy(&copies, root, 2).await,
            Err("logrotate exited 0 but did not rotate conversation.log".to_string())
        );
        assert_eq!(
            tree_bytes(root),
            vec![
                (
                    PathBuf::from("conversation.log"),
                    Some(window.clone().into_bytes())
                ),
                (
                    PathBuf::from("conversation.log.1"),
                    Some(window.into_bytes())
                ),
                (
                    PathBuf::from("conversation.log.3.gz"),
                    Some(b"archive-3-bytes".to_vec())
                ),
            ],
            "nothing is deleted"
        );

        // (v) copies conversation.log to .1, then gzip fails (disk full).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_archive_root(root);
        let seeded = tree_bytes(root);
        let gzip_fails = stub_program(
            programs.path(),
            "gzip-fails",
            &format!(
                "cp '{0}/conversation.log' '{0}/conversation.log.1'\necho 'gzip: stdout: No space left on device' >&2\nexit 1",
                root.display()
            ),
        );
        let err = archive_with_retrying_busy(&gzip_fails, root, 2)
            .await
            .expect_err("a failed gzip is an error");
        assert!(err.starts_with("logrotate failed (exit 1)"), "got {err:?}");
        assert_eq!(
            tree_bytes(root),
            seeded,
            "the window is untouched and the leftover copy is removed"
        );
    }

    // Pruning removes every archive numbered above N up to the 1000 cap and
    // nothing else; a second prune finds nothing to remove and is Ok.
    #[test]
    fn prune_archives_removes_every_archive_above_n() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_archive_root(root);
        fs::write(root.join("conversation.log.1000.gz"), "archive-1000-bytes").unwrap();

        let want = vec![
            (
                PathBuf::from("conversation.log"),
                Some(record("user", "the window").into_bytes()),
            ),
            (
                PathBuf::from("conversation.log.1.gz"),
                Some(b"archive-1-bytes".to_vec()),
            ),
            (
                PathBuf::from("conversation.log.2.gz"),
                Some(b"archive-2-bytes".to_vec()),
            ),
            (PathBuf::from("research"), None),
            (PathBuf::from("research/r1"), None),
            (
                PathBuf::from("research/r1/conversation.log"),
                Some(b"subdirectory-log\n".to_vec()),
            ),
        ];
        prune_archives(root, 2).unwrap();
        assert_eq!(tree_bytes(root), want);
        prune_archives(root, 2).expect("a second prune is Ok");
        assert_eq!(tree_bytes(root), want, "a second prune changes nothing");
    }
}
