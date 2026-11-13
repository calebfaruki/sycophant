//! The harness is the sole author of its agent's conversation.

use std::path::PathBuf;
use std::sync::Arc;

use proto_common::{text_block, Message};
use tokio::sync::{Mutex, MutexGuard, RwLock};
use tokio_util::sync::CancellationToken;

use crate::conversation::{cut_torn_tail, ConversationLog, LocalFsStore, CONVERSATION_FILENAME};
use crate::execution_log::{ExecutionLogWriter, LocalFsExecutionLog, EXECUTION_LOG_FILENAME};
use crate::queue::{Inbox, MessageQueue};

/// The end entry boot writes for a tool call the log left open.
const RESTART_TEXT: &str =
    "The agent restarted while this was running. It may or may not have finished.";

pub(crate) struct AgentState {
    log: RwLock<ConversationLog>,
    exec_log: Arc<dyn ExecutionLogWriter>,
    /// The turn slot. The message loop holds it for a whole turn; clear and
    /// compact hold it while they run, so neither splits one turn and a
    /// queued item waits until the clear or compact is done.
    slot: Mutex<()>,
    running: std::sync::Mutex<Option<CancellationToken>>,
    queue: MessageQueue,
    /// A sub-agent's messages. Empty in an agent.
    inbox: Inbox,
}

impl AgentState {
    /// Boot: cut a torn last line off `<root>/conversation.log` and
    /// `<root>/execution.json`, then build the in-memory window from
    /// `conversation.log` alone; archives are never loaded into the context.
    /// Every tool call the previous run left open gets a restart entry.
    pub(crate) async fn open(root: PathBuf, archives: u32) -> Result<Self, String> {
        for name in [CONVERSATION_FILENAME, EXECUTION_LOG_FILENAME] {
            cut_torn_tail(root.join(name)).await?;
        }
        let store = Arc::new(LocalFsStore::new(root.clone(), archives));
        let mut log = ConversationLog::rebuild(store).await?;
        log.close_open_calls(RESTART_TEXT).await?;
        Ok(Self {
            log: RwLock::new(log),
            exec_log: Arc::new(LocalFsExecutionLog::new(root)),
            slot: Mutex::new(()),
            running: std::sync::Mutex::new(None),
            queue: MessageQueue::default(),
            inbox: Inbox::default(),
        })
    }

    pub(crate) fn log(&self) -> &RwLock<ConversationLog> {
        &self.log
    }

    /// After a restart, a non-empty log gets one user entry naming the
    /// sub-agents' harnesses still alive at `paths`.
    pub(crate) async fn note_restart(&self, paths: &[String]) -> Result<(), String> {
        let mut log = self.log.write().await;
        if log.is_empty() {
            return Ok(());
        }
        let alive = if paths.is_empty() {
            "none".to_string()
        } else {
            paths.join(", ")
        };
        log.append(Message {
            role: "user".into(),
            content: vec![text_block(format!(
                "[agent restarted] Alive sub-agent dispatches: {alive}"
            ))],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        })
        .await
    }

    pub(crate) fn execution_log(&self) -> Arc<dyn ExecutionLogWriter> {
        self.exec_log.clone()
    }

    /// Replace the execution log with `writer`, for a test that inspects or
    /// gates every append.
    #[cfg(test)]
    pub(crate) fn with_execution_log(mut self, writer: Arc<dyn ExecutionLogWriter>) -> Self {
        self.exec_log = writer;
        self
    }

    pub(crate) fn queue(&self) -> &MessageQueue {
        &self.queue
    }

    pub(crate) fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// Waiters are served in arrival order (tokio's `Mutex` is fair).
    pub(crate) async fn acquire_turn_slot(&self) -> MutexGuard<'_, ()> {
        self.slot.lock().await
    }

    /// Register a fresh cancellation token for the starting turn and return a
    /// clone for the turn loop to race against.
    pub(crate) fn register_turn(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.running.lock().expect("running-turn lock poisoned") = Some(token.clone());
        token
    }

    /// Drop the running turn's token once the turn ends, so a later `cancel`
    /// while idle reports no turn.
    pub(crate) fn end_turn(&self) {
        self.running
            .lock()
            .expect("running-turn lock poisoned")
            .take();
    }

    /// Fire the running turn's cancellation token.
    pub(crate) fn cancel(&self) -> bool {
        match self
            .running
            .lock()
            .expect("running-turn lock poisoned")
            .take()
        {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_reports_true_only_for_a_turn_in_flight_and_fires_its_token() {
        let root = tempfile::TempDir::new().unwrap();
        let agent_state = AgentState::open(root.path().to_path_buf(), 10)
            .await
            .unwrap();
        assert!(!agent_state.cancel(), "an idle agent reports no turn");

        let token = agent_state.register_turn();
        assert!(!token.is_cancelled());

        let first = agent_state.cancel();
        assert!(first, "an in-flight turn is reported cancelled");
        assert!(token.is_cancelled(), "the in-flight turn's token must fire");

        let second = agent_state.cancel();
        assert!(!second, "a second cancel reports false");
        assert!(
            token.is_cancelled(),
            "the turn stays cancelled after the redundant second cancel"
        );
    }

    // At startup a last line with no closing newline (a write cut short) is
    // cut off conversation.log and execution.json before the window is
    // built: each file keeps only its whole lines, and the window holds the
    // conversation's entries. A file that is empty or ends in a newline is
    // left byte-equal, and a missing file is not created.
    #[tokio::test]
    async fn open_cuts_a_torn_last_line_before_building_the_window() {
        use std::fs;

        let good = crate::test_doubles::record("user", "good").into_bytes();
        let torn: &[u8] = b"{\"role\":\"user\",\"con";
        let good_then_torn = [good.as_slice(), torn].concat();

        // (file before, file after, window entries)
        let cases: [(&[u8], &[u8], usize); 5] = [
            (&good_then_torn, &good, 1),
            (torn, b"", 0),
            (b"", b"", 0),
            (&good, &good, 1),
            (&good[..good.len() - 1], b"", 0),
        ];
        for (before, want, entries) in cases {
            let tmp = tempfile::TempDir::new().unwrap();
            let path = tmp.path().join("conversation.log");
            let exec = tmp.path().join("execution.json");
            fs::write(&path, before).unwrap();
            fs::write(&exec, before).unwrap();
            let agent_state = AgentState::open(tmp.path().to_path_buf(), 10)
                .await
                .expect("open");
            for file in [&path, &exec] {
                assert_eq!(
                    fs::read(file).unwrap(),
                    want,
                    "{} after open, from {:?}",
                    file.display(),
                    String::from_utf8_lossy(before)
                );
            }
            assert_eq!(
                agent_state.log().read().await.history_for_provider().len(),
                entries,
                "window entries after open, from {:?}",
                String::from_utf8_lossy(before)
            );
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let agent_state = AgentState::open(tmp.path().to_path_buf(), 10)
            .await
            .expect("open with no conversation.log");
        assert!(agent_state.log().read().await.is_empty());
        for name in ["conversation.log", "execution.json"] {
            assert!(!tmp.path().join(name).exists(), "open creates no {name}");
        }
    }

    // At startup every tool call in conversation.log with no end entry gets one
    // restart entry, in call order, on disk by the time open returns. Here one
    // call finished, one was waiting for the user's approval and one was
    // running when the harness died. A second open finds nothing open and adds
    // nothing, so each call ends with exactly one entry.
    #[tokio::test]
    async fn open_writes_one_restart_entry_per_open_call_and_none_on_a_second_open() {
        use crate::conversation::{parse_ndjson, AssistantAttribution, LogEntry};
        use proto_common::{content_text, text_block, Message, ToolCall};

        const RESTART: &str =
            "The agent restarted while this was running. It may or may not have finished.";

        let tmp = tempfile::TempDir::new().unwrap();
        let call = |id: &str| ToolCall {
            id: id.into(),
            name: "Shell".into(),
            input_json: "{}".into(),
        };
        {
            let mut log =
                ConversationLog::new(Arc::new(LocalFsStore::new(tmp.path().to_path_buf(), 10)));
            log.append(Message {
                role: "user".into(),
                content: vec![text_block("run them".into())],
                tool_calls: vec![],
                tool_call_id: None,
                is_error: None,
            })
            .await
            .unwrap();
            log.append_assistant(
                Message {
                    role: "assistant".into(),
                    content: vec![],
                    tool_calls: vec![call("finished"), call("awaiting-approval"), call("running")],
                    tool_call_id: None,
                    is_error: None,
                },
                AssistantAttribution::default(),
            )
            .await
            .unwrap();
            log.append(Message {
                role: "tool".into(),
                content: vec![text_block("done".into())],
                tool_calls: vec![],
                tool_call_id: Some("finished".into()),
                is_error: None,
            })
            .await
            .unwrap();
        }

        let conv = tmp.path().join("conversation.log");
        let on_disk = || -> Vec<LogEntry> {
            parse_ndjson(&std::fs::read_to_string(&conv).unwrap(), "conversation.log")
        };

        let _first = AgentState::open(tmp.path().to_path_buf(), 10)
            .await
            .unwrap();
        let entries = on_disk();
        let added: Vec<(String, Option<String>, String)> = entries
            .iter()
            .skip(3)
            .map(|e| {
                (
                    e.role.clone(),
                    e.tool_call_id.clone(),
                    content_text(&e.content),
                )
            })
            .collect();
        assert_eq!(
            added,
            vec![
                (
                    "tool".to_string(),
                    Some("awaiting-approval".to_string()),
                    RESTART.to_string()
                ),
                (
                    "tool".to_string(),
                    Some("running".to_string()),
                    RESTART.to_string()
                ),
            ],
            "open must write one restart entry per open call, in call order, and nothing for \
             the finished call"
        );

        let second = AgentState::open(tmp.path().to_path_buf(), 10)
            .await
            .unwrap();
        assert_eq!(
            on_disk().len(),
            5,
            "a second open must find no open call and write nothing"
        );
        assert_eq!(
            second.log().read().await.history_for_provider().len(),
            5,
            "the window after a second open holds each call's single end entry"
        );
    }

    #[tokio::test]
    async fn a_restart_note_names_the_alive_paths_or_none_only_in_a_non_empty_log_and_queues_nothing(
    ) {
        use crate::conversation::{parse_ndjson, LogEntry};
        use crate::test_doubles::record;
        use proto_common::content_text;

        let empty = tempfile::TempDir::new().unwrap();
        let state = AgentState::open(empty.path().to_path_buf(), 10)
            .await
            .unwrap();
        state.note_restart(&["sub/0a1b2c3d".into()]).await.unwrap();
        let window = state.log().read().await.history_for_provider().len();
        assert_eq!(window, 0, "an empty log gets no note");

        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("conversation.log"), record("user", "hi")).unwrap();
        let alive = ["sub/0a1b2c3d".to_string(), "two/12345678".to_string()];
        let first = AgentState::open(tmp.path().to_path_buf(), 10)
            .await
            .unwrap();
        first.note_restart(&alive).await.unwrap();
        let second = AgentState::open(tmp.path().to_path_buf(), 10)
            .await
            .unwrap();
        second.note_restart(&[]).await.unwrap();

        let conv = std::fs::read_to_string(tmp.path().join("conversation.log")).unwrap();
        let entries: Vec<(String, String)> = parse_ndjson::<LogEntry>(&conv, "conversation.log")
            .iter()
            .map(|e| (e.role.clone(), content_text(&e.content)))
            .collect();
        let entry = |text: &str| ("user".to_string(), text.to_string());
        assert_eq!(
            entries,
            [
                entry("hi"),
                entry(
                    "[agent restarted] Alive sub-agent dispatches: \
                     sub/0a1b2c3d, two/12345678"
                ),
                entry("[agent restarted] Alive sub-agent dispatches: none"),
            ],
            "each start writes one note naming the alive paths, or none"
        );
        assert!(
            second.queue().pop().await.is_none(),
            "a note queues nothing"
        );
    }
}
