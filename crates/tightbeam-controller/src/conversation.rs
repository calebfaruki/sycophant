use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use tightbeam_providers::types::{ContentBlock, Message, ToolCall};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
}

#[derive(Debug, Clone)]
struct Entry {
    message: Message,
    tag: Option<String>,
}

fn is_filtered_tag(tag: &Option<String>) -> bool {
    tag.as_deref()
        .is_some_and(|t| t.starts_with("system_agent_"))
}

pub struct ConversationLog {
    entries: Vec<Entry>,
    system_prompt: Option<String>,
    log_path: PathBuf,
}

impl ConversationLog {
    pub fn new(log_dir: &Path) -> Self {
        let log_path = log_dir.join("conversation.ndjson");
        Self {
            entries: Vec::new(),
            system_prompt: None,
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
                        agent: log_entry.agent,
                    },
                    tag: log_entry.tag,
                });
            }
        }

        Ok(Self {
            entries,
            system_prompt: None,
            log_path,
        })
    }

    pub fn set_system_prompt(&mut self, prompt: String) {
        self.system_prompt = Some(prompt);
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub fn append(&mut self, message: Message) -> Result<(), String> {
        self.append_tagged(message, None)
    }

    pub fn append_tagged(&mut self, message: Message, tag: Option<String>) -> Result<(), String> {
        let entry = Entry { message, tag };
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

    pub fn history_for_provider(&self) -> Vec<Message> {
        let visible: Vec<&Message> = self
            .entries
            .iter()
            .filter(|e| !is_filtered_tag(&e.tag))
            .map(|e| &e.message)
            .collect();

        let agents: HashSet<&str> = visible.iter().filter_map(|m| m.agent.as_deref()).collect();

        if agents.len() < 2 {
            return visible.into_iter().cloned().collect();
        }

        visible
            .into_iter()
            .map(|m| {
                if m.role != "assistant" {
                    return m.clone();
                }
                let agent_name = match &m.agent {
                    Some(name) => name,
                    None => return m.clone(),
                };
                let mut msg = m.clone();
                if let Some(ref mut blocks) = msg.content {
                    if let Some(ContentBlock::Text { ref mut text }) = blocks.first_mut() {
                        *text = format!("[{agent_name}]: {text}");
                    }
                }
                msg
            })
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
            agent: entry.message.agent.clone(),
            tag: entry.tag.clone(),
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
            agent: None,
        }
    }

    #[test]
    fn new_log_starts_empty() {
        let tmp = TempDir::new().unwrap();
        let log = ConversationLog::new(tmp.path());
        assert!(log.history().is_empty());
        assert!(log.system_prompt().is_none());
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
    fn system_prompt_updates_on_each_set() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.set_system_prompt("You are helpful.".into());
        assert_eq!(log.system_prompt(), Some("You are helpful."));

        log.set_system_prompt("Updated.".into());
        assert_eq!(log.system_prompt(), Some("Updated."));
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
            agent: None,
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
            agent: None,
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
    fn agent_attribution_round_trips_through_rebuild() {
        let tmp = TempDir::new().unwrap();
        {
            let mut log = ConversationLog::new(tmp.path());
            log.append(text_msg("user", "Hello")).unwrap();
            let mut assistant = text_msg("assistant", "Hi there");
            assistant.agent = Some("research".into());
            log.append(assistant).unwrap();
        }

        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
        assert_eq!(rebuilt.history().len(), 2);
        assert_eq!(rebuilt.history()[1].agent.as_deref(), Some("research"));
    }

    #[test]
    fn history_for_provider_no_prefix_single_agent() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append(text_msg("user", "Hello")).unwrap();
        let mut msg = text_msg("assistant", "Hi there");
        msg.agent = Some("research".into());
        log.append(msg).unwrap();

        let history = log.history_for_provider();
        assert_eq!(
            content_text(&history[1].content),
            Some("Hi there"),
            "single agent should not prefix"
        );
    }

    #[test]
    fn history_for_provider_prefixes_multi_agent() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append(text_msg("user", "Hello")).unwrap();

        let mut msg1 = text_msg("assistant", "Analysis here");
        msg1.agent = Some("research".into());
        log.append(msg1).unwrap();

        log.append(text_msg("user", "Write it up")).unwrap();

        let mut msg2 = text_msg("assistant", "Draft here");
        msg2.agent = Some("writer".into());
        log.append(msg2).unwrap();

        let history = log.history_for_provider();
        assert_eq!(
            content_text(&history[1].content),
            Some("[research]: Analysis here"),
        );
        assert_eq!(
            content_text(&history[3].content),
            Some("[writer]: Draft here"),
        );
        assert_eq!(
            content_text(&history[0].content),
            Some("Hello"),
            "user messages should not be prefixed"
        );
    }

    #[test]
    fn history_for_provider_drops_system_agent_response() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append(text_msg("user", "Hi")).unwrap();
        let mut router_reply = text_msg("assistant", "alice");
        router_reply.agent = Some("system".into());
        log.append_tagged(router_reply, Some("system_agent_response".into()))
            .unwrap();

        let history = log.history_for_provider();
        assert_eq!(
            history.len(),
            1,
            "system_agent_response must be filtered out"
        );
        assert_eq!(
            history[0].role, "user",
            "filtered last entry must be the user message"
        );
    }

    #[test]
    fn history_for_provider_keeps_real_agent_assistant() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append(text_msg("user", "Hi")).unwrap();
        let mut router_reply = text_msg("assistant", "alice");
        router_reply.agent = Some("system".into());
        log.append_tagged(router_reply, Some("system_agent_response".into()))
            .unwrap();
        let mut alice_reply = text_msg("assistant", "Hi I'm alice");
        alice_reply.agent = Some("alice".into());
        log.append(alice_reply).unwrap();
        log.append(text_msg("user", "and what about tests?"))
            .unwrap();

        let history = log.history_for_provider();
        assert_eq!(history.len(), 3, "expected user, agent, user");
        assert_eq!(history[0].role, "user");
        assert_eq!(history[1].role, "assistant");
        assert_eq!(history[1].agent.as_deref(), Some("alice"));
        assert_eq!(history[2].role, "user", "last role must be user");
    }

    #[test]
    fn history_for_provider_multi_agent_prefix_excludes_system_from_count() {
        let tmp = TempDir::new().unwrap();
        let mut log = ConversationLog::new(tmp.path());

        log.append(text_msg("user", "Hi")).unwrap();
        let mut router_reply = text_msg("assistant", "alice");
        router_reply.agent = Some("system".into());
        log.append_tagged(router_reply, Some("system_agent_response".into()))
            .unwrap();
        let mut alice_reply = text_msg("assistant", "Analysis");
        alice_reply.agent = Some("alice".into());
        log.append(alice_reply).unwrap();
        log.append(text_msg("user", "And bob's view?")).unwrap();
        let mut bob_reply = text_msg("assistant", "Critique");
        bob_reply.agent = Some("bob".into());
        log.append(bob_reply).unwrap();

        let history = log.history_for_provider();
        assert_eq!(
            content_text(&history[1].content),
            Some("[alice]: Analysis"),
            "alice should be prefixed because filtered set has 2 named agents"
        );
        assert_eq!(content_text(&history[3].content), Some("[bob]: Critique"));
    }

    #[test]
    fn rebuild_round_trips_tag() {
        let tmp = TempDir::new().unwrap();

        {
            let mut log = ConversationLog::new(tmp.path());
            log.append(text_msg("user", "Hi")).unwrap();
            let mut router_reply = text_msg("assistant", "alice");
            router_reply.agent = Some("system".into());
            log.append_tagged(router_reply, Some("system_agent_response".into()))
                .unwrap();
        }

        let rebuilt = ConversationLog::rebuild(tmp.path()).unwrap();
        let history = rebuilt.history_for_provider();
        assert_eq!(
            history.len(),
            1,
            "after rebuild, system_agent_response must still be filtered"
        );
        assert_eq!(history[0].role, "user");
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
        let history = rebuilt.history_for_provider();
        assert_eq!(
            history.len(),
            1,
            "untagged legacy entry must remain visible"
        );
    }
}
