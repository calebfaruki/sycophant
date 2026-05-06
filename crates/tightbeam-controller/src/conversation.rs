use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use tightbeam_providers::types::{ContentBlock, Message, ToolCall};

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
struct LogEntry {
    ts: String,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Vec<ContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_prompt_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AssistantAttribution {
    pub model: Option<String>,
    pub system_prompt_sha256: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct Entry {
    message: Message,
    tag: Option<String>,
    attribution: AssistantAttribution,
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
    role: Option<tightbeam_proto::TurnRole>,
    correlation_id: Option<&str>,
) -> Option<String> {
    use tightbeam_proto::TurnRole;
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
    log_path: PathBuf,
}

impl ConversationLog {
    pub fn new(log_dir: &Path) -> Self {
        let log_path = log_dir.join("conversation.ndjson");
        Self {
            entries: Vec::new(),
            log_path,
        }
    }

    pub fn rebuild(log_dir: &Path) -> Result<Self, String> {
        let log_path = log_dir.join("conversation.ndjson");
        let mut entries = Vec::new();

        if log_path.exists() {
            let file = fs::File::open(&log_path).map_err(|e| format!("failed to open log: {e}"))?;
            let reader = BufReader::new(file);

            for line in reader.lines() {
                let line = line.map_err(|e| format!("failed to read log line: {e}"))?;
                if line.is_empty() {
                    continue;
                }
                let log_entry: LogEntry = serde_json::from_str(&line)
                    .map_err(|e| format!("failed to parse log entry: {e}"))?;
                entries.push(Entry {
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
                });
            }
        }

        Ok(Self { entries, log_path })
    }

    pub fn append(&mut self, message: Message) -> Result<(), String> {
        self.append_tagged(message, None)
    }

    pub fn append_tagged(&mut self, message: Message, tag: Option<String>) -> Result<(), String> {
        let entry = Entry {
            message,
            tag,
            attribution: AssistantAttribution::default(),
        };
        Self::write_entry(&self.log_path, &entry)?;
        self.entries.push(entry);
        Ok(())
    }

    /// Append an assistant entry with attribution metadata (model that ran the
    /// call, hash of the system prompt the LLM was given, optional agent name
    /// for delegate calls). Use this from the LLM-result-streaming path; user
    /// and tool entries should keep using [`append_tagged`].
    pub fn append_assistant_tagged(
        &mut self,
        message: Message,
        tag: Option<String>,
        attribution: AssistantAttribution,
    ) -> Result<(), String> {
        let entry = Entry {
            message,
            tag,
            attribution,
        };
        Self::write_entry(&self.log_path, &entry)?;
        self.entries.push(entry);
        Ok(())
    }

    pub fn append_many(&mut self, messages: Vec<Message>) -> Result<(), String> {
        for message in messages {
            self.append(message)?;
        }
        Ok(())
    }

    pub fn append_many_tagged(
        &mut self,
        messages: Vec<Message>,
        tag: Option<String>,
    ) -> Result<(), String> {
        for message in messages {
            self.append_tagged(message, tag.clone())?;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn truncate(&mut self, len: usize) {
        if len >= self.entries.len() {
            return;
        }
        self.entries.truncate(len);
        self.rewrite_log();
    }

    fn rewrite_log(&self) {
        if let Err(e) = self.rewrite_log_inner() {
            tracing::error!("failed to rewrite conversation log: {e}");
        }
    }

    fn rewrite_log_inner(&self) -> Result<(), String> {
        let tmp_path = self.log_path.with_extension("ndjson.tmp");
        let mut file =
            fs::File::create(&tmp_path).map_err(|e| format!("failed to create temp log: {e}"))?;
        for entry in &self.entries {
            let log_entry = Self::entry_to_log_entry(entry);
            let mut line = serde_json::to_string(&log_entry)
                .map_err(|e| format!("failed to serialize: {e}"))?;
            line.push('\n');
            file.write_all(line.as_bytes())
                .map_err(|e| format!("failed to write: {e}"))?;
        }
        fs::rename(&tmp_path, &self.log_path).map_err(|e| format!("failed to rename: {e}"))?;
        Ok(())
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
            ts: Utc::now().to_rfc3339(),
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

    fn write_entry(path: &Path, entry: &Entry) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("failed to create log dir: {e}"))?;
        }

        let log_entry = Self::entry_to_log_entry(entry);

        let mut line = serde_json::to_string(&log_entry)
            .map_err(|e| format!("failed to serialize log entry: {e}"))?;
        line.push('\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("failed to open log file: {e}"))?;

        file.write_all(line.as_bytes())
            .map_err(|e| format!("failed to write log entry: {e}"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tightbeam_providers::types::content_text;

    fn text_msg(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: Some(ContentBlock::text_content(text)),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
        }
    }

    #[test]
    fn new_log_starts_empty() {
        let tmp = TempDir::new().unwrap();
        let log = ConversationLog::new(tmp.path());
        assert!(log.history().is_empty());
    }

    #[test]
    fn append_adds_to_history_and_log_file() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append(text_msg("user", "Hello")).unwrap();
        log.append(text_msg("assistant", "Hi there")).unwrap();

        assert_eq!(log.history().len(), 2);
        assert_eq!(log.history()[0].role, "user");
        assert_eq!(log.history()[1].role, "assistant");

        let log_file = tmp.path().join("conversation.ndjson");
        assert!(log_file.exists());
        let content = fs::read_to_string(&log_file).unwrap();
        let lines: Vec<&str> = content.trim().split('\n').collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn rebuild_restores_history_from_log() {
        let tmp = TempDir::new().unwrap();

        {
            let mut log = ConversationLog::new(tmp.path());
            log.append(text_msg("user", "First")).unwrap();
            log.append(text_msg("assistant", "Second")).unwrap();
            log.append(text_msg("user", "Third")).unwrap();
        }

        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
        assert_eq!(rebuilt.history().len(), 3);
        assert_eq!(rebuilt.history()[0].role, "user");
        assert_eq!(rebuilt.history()[1].role, "assistant");
        assert_eq!(rebuilt.history()[2].role, "user");
    }

    #[test]
    fn rebuild_empty_dir_returns_empty_log() {
        let tmp = TempDir::new().unwrap();
        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
        assert!(rebuilt.history().is_empty());
    }

    #[test]
    fn tool_result_message_round_trips() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        let msg = Message {
            role: "tool".into(),
            content: Some(ContentBlock::text_content("ls output")),
            tool_calls: None,
            tool_call_id: Some("tc-001".into()),
            is_error: None,
        };
        log.append(msg).unwrap();

        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
        assert_eq!(rebuilt.history().len(), 1);
        assert_eq!(rebuilt.history()[0].role, "tool");
        assert_eq!(rebuilt.history()[0].tool_call_id.as_deref(), Some("tc-001"));
    }

    #[test]
    fn assistant_with_tool_calls_round_trips() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

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
        log.append(msg).unwrap();

        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
        let history = rebuilt.history();
        let tool_calls = history[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "bash");
    }

    #[test]
    fn truncate_rolls_back_history_and_log() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append(text_msg("user", "First")).unwrap();
        log.append(text_msg("assistant", "Second")).unwrap();
        log.append(text_msg("user", "Third")).unwrap();
        assert_eq!(log.len(), 3);

        log.truncate(1);
        assert_eq!(log.len(), 1);
        assert_eq!(log.history()[0].role, "user");

        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
        assert_eq!(rebuilt.history().len(), 1);
    }

    #[test]
    fn rebuild_fails_on_corrupted_log() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("conversation.ndjson");
        std::fs::write(
            &log_path,
            "{\"ts\":\"t\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}\nnot json\n",
        )
        .unwrap();
        assert!(
            ConversationLog::rebuild(tmp.path()).is_err(),
            "should fail on corrupted log entry"
        );
    }

    #[test]
    fn delegate_scope_isolates_per_call_and_orchestrator_excludes_them() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        // Orchestrator user input
        log.append(text_msg("user", "do thing")).unwrap();
        // Orchestrator assistant tool_use (untagged)
        log.append(text_msg("assistant", "calling tool")).unwrap();
        // Delegate call A
        log.append_tagged(
            text_msg("user", "delegate A query"),
            Some("delegate:call-A".into()),
        )
        .unwrap();
        log.append_tagged(
            text_msg("assistant", "delegate A reply"),
            Some("delegate:call-A".into()),
        )
        .unwrap();
        // Delegate call B
        log.append_tagged(
            text_msg("user", "delegate B query"),
            Some("delegate:call-B".into()),
        )
        .unwrap();
        log.append_tagged(
            text_msg("assistant", "delegate B reply"),
            Some("delegate:call-B".into()),
        )
        .unwrap();
        // Orchestrator final reply (untagged)
        log.append(text_msg("assistant", "final")).unwrap();

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

    #[test]
    fn assistant_attribution_round_trips_through_rebuild() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log = ConversationLog::new(tmp.path());
            log.append(text_msg("user", "hi")).unwrap();
            log.append_assistant_tagged(
                text_msg("assistant", "hello back"),
                None,
                AssistantAttribution {
                    model: Some("default".into()),
                    system_prompt_sha256: Some(sha256_hex("You are helpful.")),
                    warnings: vec![],
                },
            )
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
            .unwrap();
        }

        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
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

    #[test]
    fn sha256_hex_is_stable() {
        // Lowercase hex, 64 chars for SHA-256.
        let h = sha256_hex("abc");
        assert_eq!(h.len(), 64);
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn frontmatter_passthrough_when_absent() {
        let input = "You are a helpful assistant.";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, input);
        assert!(fm.model.is_none());
    }

    #[test]
    fn frontmatter_extracts_model_and_strips() {
        let input = "---\nmodel: smart\n---\nYou are Alice.";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "You are Alice.");
        assert_eq!(fm.model.as_deref(), Some("smart"));
    }

    #[test]
    fn frontmatter_strips_even_when_model_missing() {
        let input = "---\nname: alice\ndescription: warm\n---\nYou are Alice.";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "You are Alice.");
        assert!(fm.model.is_none());
    }

    #[test]
    fn frontmatter_handles_crlf() {
        let input = "---\r\nmodel: smart\r\n---\r\nYou are Alice.";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "You are Alice.");
        assert_eq!(fm.model.as_deref(), Some("smart"));
    }

    #[test]
    fn frontmatter_strips_utf8_bom() {
        let input = "\u{FEFF}---\nmodel: smart\n---\nbody";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "body");
        assert_eq!(fm.model.as_deref(), Some("smart"));
    }

    #[test]
    fn frontmatter_passthrough_when_missing_closer() {
        // No closing --- → not actually frontmatter; pass through.
        let input = "---\nmodel: smart\nYou are Alice.";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, input);
        assert!(fm.model.is_none());
    }

    #[test]
    fn frontmatter_passthrough_when_yaml_invalid() {
        let input = "---\n: : not valid : yaml :\n---\nbody";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, input);
        assert!(fm.model.is_none());
    }

    #[test]
    fn frontmatter_ignores_non_string_model_field() {
        // model is a list, not a string → ignored, but body still stripped
        // because the frontmatter delimiters parsed cleanly.
        let input = "---\nmodel:\n  - smart\n  - fast\n---\nbody";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "body");
        assert!(fm.model.is_none());
    }

    #[test]
    fn frontmatter_passthrough_when_closer_past_scan_limit() {
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

    #[test]
    fn frontmatter_empty_body_is_permitted() {
        let input = "---\nmodel: smart\n---\n";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "");
        assert_eq!(fm.model.as_deref(), Some("smart"));
    }

    #[test]
    fn frontmatter_extracts_params_block() {
        let input = "---\nparams:\n  output_config:\n    effort: high\n  max_tokens: 16000\n---\nbody";
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

    #[test]
    fn frontmatter_without_params_returns_none() {
        let input = "---\nmodel: smart\n---\nbody";
        let (_body, fm) = strip_frontmatter(input);
        assert!(fm.params.is_none());
    }

    #[test]
    fn frontmatter_with_non_mapping_params_returns_none() {
        let input = "---\nparams: 42\n---\nbody";
        let (body, fm) = strip_frontmatter(input);
        assert_eq!(body, "body");
        assert!(fm.params.is_none());
    }

    #[test]
    fn last_assistant_model_returns_none_for_empty_log() {
        let tmp = TempDir::new().unwrap();
        let log = ConversationLog::new(tmp.path());
        assert!(log
            .last_assistant_model(HistoryScope::Orchestrator)
            .is_none());
    }

    #[test]
    fn last_assistant_model_returns_most_recent_assistant_in_orchestrator_scope() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append(text_msg("user", "hi")).unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "older"),
            None,
            AssistantAttribution {
                model: Some("haiku".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
        .unwrap();
        log.append(text_msg("user", "again")).unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "newer"),
            None,
            AssistantAttribution {
                model: Some("sonnet".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
        .unwrap();

        assert_eq!(
            log.last_assistant_model(HistoryScope::Orchestrator).as_deref(),
            Some("sonnet")
        );
    }

    #[test]
    fn last_assistant_model_skips_user_and_tool_entries() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append_assistant_tagged(
            text_msg("assistant", "earlier"),
            None,
            AssistantAttribution {
                model: Some("haiku".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
        .unwrap();
        log.append(text_msg("user", "more")).unwrap();
        log.append(text_msg("tool", "result")).unwrap();

        assert_eq!(
            log.last_assistant_model(HistoryScope::Orchestrator).as_deref(),
            Some("haiku"),
            "user and tool entries must be skipped"
        );
    }

    #[test]
    fn last_assistant_model_filters_by_delegate_scope() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append_assistant_tagged(
            text_msg("assistant", "orchestrator"),
            None,
            AssistantAttribution {
                model: Some("orchestrator-model".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
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
        .unwrap();

        assert_eq!(
            log.last_assistant_model(HistoryScope::Orchestrator).as_deref(),
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

    #[test]
    fn last_assistant_model_skips_assistants_without_model_attribution() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append_assistant_tagged(
            text_msg("assistant", "with model"),
            None,
            AssistantAttribution {
                model: Some("haiku".into()),
                system_prompt_sha256: None,
                warnings: vec![],
            },
        )
        .unwrap();
        log.append_assistant_tagged(
            text_msg("assistant", "no model"),
            None,
            AssistantAttribution::default(),
        )
        .unwrap();

        assert_eq!(
            log.last_assistant_model(HistoryScope::Orchestrator).as_deref(),
            Some("haiku"),
            "entries without model attribution must be skipped"
        );
    }

    #[test]
    fn assistant_attribution_warnings_round_trip() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log = ConversationLog::new(tmp.path());
            log.append_assistant_tagged(
                text_msg("assistant", "ok"),
                None,
                AssistantAttribution {
                    model: Some("haiku".into()),
                    system_prompt_sha256: None,
                    warnings: vec!["model".into(), "messages".into()],
                },
            )
            .unwrap();
        }
        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
        let attrs = rebuilt.attributions();
        assert_eq!(attrs[0].warnings, vec!["model".to_string(), "messages".to_string()]);
    }

    #[test]
    fn assistant_attribution_warnings_default_empty_for_legacy_log() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("conversation.ndjson");
        // Legacy log entry without warnings field.
        std::fs::write(
            &log_path,
            "{\"ts\":\"t\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}],\"model\":\"haiku\"}\n",
        )
        .unwrap();
        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
        let attrs = rebuilt.attributions();
        assert!(attrs[0].warnings.is_empty());
    }

    #[test]
    fn is_empty_reflects_actual_entry_count() {
        // Catches `replace ConversationLog::is_empty -> bool with true`.
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());
        assert!(log.is_empty(), "fresh log should be empty");

        log.append(text_msg("user", "hi")).unwrap();

        assert!(!log.is_empty(), "log with 1 entry must not report empty");
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn frontmatter_scan_limit_is_4kib() {
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

    #[test]
    fn rebuild_handles_legacy_entries_without_tag() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("conversation.ndjson");
        std::fs::write(
            &log_path,
            "{\"ts\":\"t\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Hi\"}]}\n",
        )
        .unwrap();

        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
        assert_eq!(rebuilt.history().len(), 1);
        assert_eq!(rebuilt.history()[0].role, "user");
        let history = rebuilt.history_for_provider(HistoryScope::Orchestrator);
        assert_eq!(
            history.len(),
            1,
            "untagged legacy entry must remain visible"
        );
    }
}
