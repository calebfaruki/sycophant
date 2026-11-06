//! Shared `#[cfg(test)]` test doubles reachable across the crate's unit-test
//! modules.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use proto_common::{ContentBlock, StreamItem, TurnStateEvent};
use tokio_util::sync::CancellationToken;
use toolset_proto::{turn_event, TurnEvent, TurnRequest};

use crate::clients::{RelayRpc, ServerRequestOutcome, ToolsetRpc, TurnSource};
use crate::instructions::Instructions;

/// The agent name used by test routers/instructions.
pub(crate) const TEST_AG: &str = "ag";

/// An empty-agent instructions over a throwaway temp dir. The dir is leaked so
/// the returned `Arc<Instructions>` can outlive this call; tests that need populated
/// content build their own `Instructions` directly.
pub(crate) fn test_instructions() -> Arc<Instructions> {
    let root = tempfile::TempDir::new().unwrap().keep();
    std::fs::create_dir_all(root.join(TEST_AG)).unwrap();
    Arc::new(Instructions::new(root))
}

/// Test relay that answers every call successfully and records the stream
/// items and turn-state frames (with their subject) the harness hands it.
#[derive(Default)]
pub(crate) struct RecordingRelay {
    pub(crate) stream_items: Vec<(String, StreamItem)>,
    pub(crate) turn_states: Vec<(TurnStateEvent, String)>,
}

#[async_trait]
impl RelayRpc for RecordingRelay {
    async fn send_server_notification(
        &mut self,
        _agent: &str,
        _method: &str,
        _params_json: &str,
    ) -> Result<bool, String> {
        Ok(true)
    }
    async fn send_server_request_and_await(
        &mut self,
        _agent: &str,
        _request_id: &str,
        _method: &str,
        _params_json: &str,
        _timeout_seconds: u32,
    ) -> Result<ServerRequestOutcome, String> {
        Ok(ServerRequestOutcome::Result(String::new()))
    }
    async fn deliver_stream_item(
        &mut self,
        channel_stream_id: &str,
        item: StreamItem,
    ) -> Result<bool, String> {
        self.stream_items
            .push((channel_stream_id.to_string(), item));
        Ok(true)
    }
    async fn deliver_reply(
        &mut self,
        _conversation_id: &str,
        _content: Vec<ContentBlock>,
    ) -> Result<bool, String> {
        Ok(true)
    }
    async fn deliver_turn_state(
        &mut self,
        turn_state: TurnStateEvent,
        subject: &str,
    ) -> Result<bool, String> {
        self.turn_states.push((turn_state, subject.to_string()));
        Ok(true)
    }
}

/// A shared, ordered log of what happened in a test, so a test can order
/// relay requests against pod claims and model turns.
pub(crate) type EventLog = Arc<Mutex<Vec<String>>>;

pub(crate) fn event_log() -> EventLog {
    Arc::new(Mutex::new(Vec::new()))
}

/// How the scripted relay answers one `send_server_request_and_await`.
pub(crate) enum RelayAnswer {
    Outcome(ServerRequestOutcome),
    Err(String),
    /// Never answers; only the caller dropping the future ends the wait.
    Never,
    /// Fires the token, then answers `{"approved":true}` in the same poll.
    ApproveAndCancel(CancellationToken),
}

impl RelayAnswer {
    pub(crate) fn approve() -> Self {
        Self::Outcome(ServerRequestOutcome::Result(r#"{"approved":true}"#.into()))
    }
    pub(crate) fn deny() -> Self {
        Self::Outcome(ServerRequestOutcome::Result(r#"{"approved":false}"#.into()))
    }
}

/// One `send_server_request_and_await` the relay received.
#[derive(Clone, Debug)]
pub(crate) struct RelayRequest {
    pub(crate) agent: String,
    pub(crate) method: String,
    pub(crate) params_json: String,
    pub(crate) timeout_seconds: u32,
}

#[derive(Default)]
struct ScriptedRelayState {
    script: VecDeque<RelayAnswer>,
    requests: Vec<RelayRequest>,
}

/// A relay whose clones share one record and one script. Each
/// `send_server_request_and_await` is recorded, logged as
/// `request <method>` to the event log when one is set, and answered by the
/// next script entry. An empty script answers `Result("{}")`.
#[derive(Clone, Default)]
pub(crate) struct ScriptedRelay {
    state: Arc<Mutex<ScriptedRelayState>>,
    events: Option<EventLog>,
}

impl ScriptedRelay {
    pub(crate) fn new(script: Vec<RelayAnswer>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ScriptedRelayState {
                script: script.into(),
                ..Default::default()
            })),
            events: None,
        }
    }

    pub(crate) fn with_events(mut self, events: EventLog) -> Self {
        self.events = Some(events);
        self
    }

    /// The requests received for `method`, in order.
    pub(crate) fn requests_for(&self, method: &str) -> Vec<RelayRequest> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.method == method)
            .cloned()
            .collect()
    }
}

#[async_trait]
impl RelayRpc for ScriptedRelay {
    async fn send_server_notification(
        &mut self,
        _agent: &str,
        _method: &str,
        _params_json: &str,
    ) -> Result<bool, String> {
        Ok(true)
    }
    async fn send_server_request_and_await(
        &mut self,
        agent: &str,
        _request_id: &str,
        method: &str,
        params_json: &str,
        timeout_seconds: u32,
    ) -> Result<ServerRequestOutcome, String> {
        let answer = {
            let mut state = self.state.lock().unwrap();
            state.requests.push(RelayRequest {
                agent: agent.to_string(),
                method: method.to_string(),
                params_json: params_json.to_string(),
                timeout_seconds,
            });
            state.script.pop_front()
        };
        if let Some(events) = &self.events {
            events.lock().unwrap().push(format!("request {method}"));
        }
        match answer {
            None => Ok(ServerRequestOutcome::Result("{}".into())),
            Some(RelayAnswer::Outcome(outcome)) => Ok(outcome),
            Some(RelayAnswer::Err(e)) => Err(e),
            Some(RelayAnswer::Never) => std::future::pending().await,
            Some(RelayAnswer::ApproveAndCancel(token)) => {
                token.cancel();
                Ok(ServerRequestOutcome::Result(r#"{"approved":true}"#.into()))
            }
        }
    }
    async fn deliver_stream_item(
        &mut self,
        _channel_stream_id: &str,
        _item: StreamItem,
    ) -> Result<bool, String> {
        Ok(true)
    }
    async fn deliver_reply(
        &mut self,
        _conversation_id: &str,
        _content: Vec<ContentBlock>,
    ) -> Result<bool, String> {
        Ok(true)
    }
    async fn deliver_turn_state(
        &mut self,
        _turn_state: TurnStateEvent,
        _subject: &str,
    ) -> Result<bool, String> {
        Ok(true)
    }
}

/// A model seam whose clones share one script of turns. Each `turn` call
/// replays the next scripted event list. An exhausted script fails the turn.
#[derive(Clone, Default)]
pub(crate) struct ScriptedTurns {
    turns: Arc<Mutex<VecDeque<Vec<TurnEvent>>>>,
}

impl ScriptedTurns {
    pub(crate) fn new(turns: Vec<Vec<TurnEvent>>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
        }
    }
}

#[async_trait]
impl ToolsetRpc for ScriptedTurns {
    async fn turn(&mut self, _request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
        let events = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "ScriptedTurns: no more scripted turns".to_string())?;
        Ok(Box::new(ScriptedSource {
            events: events.into(),
        }))
    }

    async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
        Ok(())
    }
}

pub(crate) struct ScriptedSource {
    pub(crate) events: VecDeque<TurnEvent>,
}

#[async_trait]
impl TurnSource for ScriptedSource {
    async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
        self.events.pop_front().map(Ok)
    }
}

/// A model turn that ends with `stop` after proposing `calls`, each given as
/// `(id, name, input_json)`.
pub(crate) fn complete_turn(
    stop: proto_common::StopReason,
    text: &str,
    calls: &[(&str, &str, &str)],
) -> Vec<TurnEvent> {
    let content = if text.is_empty() {
        vec![]
    } else {
        vec![crate::agent::text_block(text.to_string())]
    };
    vec![TurnEvent {
        event: Some(turn_event::Event::Complete(toolset_proto::TurnComplete {
            stop_reason: stop as i32,
            content,
            tool_calls: calls
                .iter()
                .map(|(id, name, input)| proto_common::ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    input_json: input.to_string(),
                })
                .collect(),
        })),
    }]
}
