//! The harness is the sole author of its agent's conversation.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, MutexGuard, RwLock};
use tokio_util::sync::CancellationToken;

use crate::conversation::{cut_torn_tail, ConversationLog, LocalFsStore, CONVERSATION_FILENAME};
use crate::execution_log::{ExecutionLogWriter, LocalFsExecutionLog, EXECUTION_LOG_FILENAME};
use crate::queue::MessageQueue;

pub(crate) struct AgentState {
    log: RwLock<ConversationLog>,
    exec_log: Arc<dyn ExecutionLogWriter>,
    /// The turn slot. The message loop holds it for a whole turn; clear and
    /// compact hold it while they run, so neither splits one turn and a
    /// queued item waits until the clear or compact is done.
    slot: Mutex<()>,
    running: std::sync::Mutex<Option<CancellationToken>>,
    queue: MessageQueue,
}

impl AgentState {
    /// Boot: cut a torn last line off `<root>/conversation.log` and
    /// `<root>/execution.json`, then build the in-memory window from
    /// `conversation.log` alone; archives are never loaded into the context.
    pub(crate) async fn load(root: PathBuf, archives: u32) -> Result<Self, String> {
        for name in [CONVERSATION_FILENAME, EXECUTION_LOG_FILENAME] {
            cut_torn_tail(root.join(name)).await?;
        }
        let store = Arc::new(LocalFsStore::new(root.clone(), archives));
        let log = ConversationLog::rebuild(store).await?;
        Ok(Self {
            log: RwLock::new(log),
            exec_log: Arc::new(LocalFsExecutionLog::new(root)),
            slot: Mutex::new(()),
            running: std::sync::Mutex::new(None),
            queue: MessageQueue::default(),
        })
    }

    pub(crate) fn log(&self) -> &RwLock<ConversationLog> {
        &self.log
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
        let agent_state = AgentState::load(root.path().to_path_buf(), 10)
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
    async fn load_cuts_a_torn_last_line_before_building_the_window() {
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
            let agent_state = AgentState::load(tmp.path().to_path_buf(), 10)
                .await
                .expect("load");
            for file in [&path, &exec] {
                assert_eq!(
                    fs::read(file).unwrap(),
                    want,
                    "{} after load, from {:?}",
                    file.display(),
                    String::from_utf8_lossy(before)
                );
            }
            assert_eq!(
                agent_state.log().read().await.history_for_provider().len(),
                entries,
                "window entries after load, from {:?}",
                String::from_utf8_lossy(before)
            );
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let agent_state = AgentState::load(tmp.path().to_path_buf(), 10)
            .await
            .expect("load with no conversation.log");
        assert!(agent_state.log().read().await.is_empty());
        for name in ["conversation.log", "execution.json"] {
            assert!(!tmp.path().join(name).exists(), "load creates no {name}");
        }
    }
}
