//! Message loop: one turn per queued message.
//!
//! A feeder drains the relay `Subscribe` stream into the harness's queue as
//! messages arrive. The turn loop takes the oldest queued message and:
//! 1. Reads the primary agent (`AGENTS.md`) fresh from this agent's
//!    mounted instructions volume. An instructions edit takes effect on the next turn with
//!    no pod restart.
//! 2. Appends the message to the conversation log, then builds a
//!    `TurnRequest` with the AGENTS.md body plus the instruction-tree map as
//!    `system`, the full window, and the full tool set. Anthropic treats each
//!    request as stateless, so tools must be sent on every turn —
//!    `agent::llm_loop` propagates them through tool-result continuations.
//! 3. Hands to `agent::llm_loop`.

use std::sync::Arc;

use proto_common::{text_block, ContentBlock, Message, ToolDefinition, TurnState, TurnStateEvent};
use toolset_proto::TurnRequest;

use crate::agent::{self, LoopError, LoopHalt, LoopMode};
use crate::agent_state::AgentState;
use crate::clients::{InferenceRpc, RelayRpc};
use crate::conversation::{sha256_hex, AssistantAttribution};
use crate::instructions::{strip_frontmatter, Instructions};
use crate::message_source::MessageSource;
use crate::queue::MessageQueue;
use crate::runtime_tools::LIST_CAP;
use crate::tool_router::ToolRouter;
use crate::turn::StreamSink;

/// Read the primary agent (`AGENTS.md`) for this turn. A missing or
/// unreadable file yields an empty system prompt rather than dropping the
/// inbound message: the turn still runs, just without an agent.
fn resolve_primary_agent(instructions: &Instructions, agent: &str) -> String {
    match instructions.read(agent, "AGENTS.md") {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "read primary agent failed, using empty system prompt");
            String::new()
        }
    }
}

/// Compose the model-facing primary system prompt: the AGENTS.md entry body
/// followed by a flat, capped map of the agent instruction tree's paths.
/// The map lets the primary turn see (and `ReadInstructions`) nested files it
/// would otherwise not know exist. An empty tree renders the body alone.
fn render_primary_prompt(agents_body: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return agents_body.to_string();
    }
    let mut prompt = agents_body.to_string();
    prompt.push_str("\n\n## Agent files\n\n");
    prompt.push_str(&paths.join("\n"));
    prompt
}

/// The per-turn inputs the primary turn sends: the system prompt (AGENTS.md
/// body with its frontmatter stripped, plus the tree map) and every
/// advertised tool. Nothing is read from the frontmatter.
fn turn_inputs(
    primary_agent_text: &str,
    tree: &[String],
    tool_router: &ToolRouter,
) -> (String, Vec<ToolDefinition>) {
    let body = strip_frontmatter(primary_agent_text);
    let system_prompt = render_primary_prompt(&body, tree);
    (system_prompt, tool_router.tool_definitions())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn message_loop(
    max_iterations: u32,
    idle_gap: std::time::Duration,
    model: Option<String>,
    inference: &mut dyn InferenceRpc,
    relay: &mut impl RelayRpc,
    instructions: &Instructions,
    agent: &str,
    tool_router: Arc<ToolRouter>,
    agent_state: Arc<AgentState>,
    message_source: &mut dyn MessageSource,
) -> Result<(), String> {
    let feeder = feed_queue(message_source, agent_state.queue());
    // Take the oldest queued message, run one turn for it, and only then take
    // the next. The message is popped only after the turn slot is taken, so
    // while a clear or compact runs it stays queued and its sender can still
    // take it back.
    let turns = async {
        loop {
            agent_state.queue().wait_ready().await;
            let _slot = agent_state.acquire_turn_slot().await;
            let Some(item) = agent_state.queue().pop().await else {
                // A take-back emptied the queue while the slot was awaited.
                continue;
            };
            // Per-turn cancellation token, fired by a client CancelTurn from the
            // moment the message is taken.
            let cancel = agent_state.register_turn();

            let primary_agent_text = resolve_primary_agent(instructions, agent);
            tracing::info!(bytes = primary_agent_text.len(), "read primary agent");

            let message_id = item.message_id;
            let reply_channel_stream = item.reply_channel_stream;
            tracing::info!(
                sender = %item.sender,
                message_id = %message_id,
                reply_channel_stream = ?reply_channel_stream,
                "turn start"
            );

            let log = agent_state.log();

            let (system_prompt, tool_defs) = turn_inputs(
                &primary_agent_text,
                &instructions.list_tree(agent, LIST_CAP),
                &tool_router,
            );

            let user_msg = Message {
                role: "user".into(),
                content: item.content,
                tool_calls: vec![],
                tool_call_id: None,
                is_error: None,
            };
            log.write()
                .await
                .append(user_msg)
                .await
                .map_err(|e| format!("append user message: {e}"))?;
            let history: Vec<Message> = log.read().await.history_for_provider();

            let prompt_hash = sha256_hex(primary_agent_text.as_bytes());
            let attribution = AssistantAttribution {
                model: model.clone(),
                system_prompt_sha256: Some(prompt_hash),
                warnings: vec![],
            };

            if reply_channel_stream.is_some() {
                let start = turn_start_frame(&message_id);
                if let Err(e) = relay.deliver_turn_state(start).await {
                    tracing::warn!(error = %e, "failed to deliver turn-start frame");
                }
            }

            let request = TurnRequest {
                system: Some(system_prompt),
                tools: tool_defs,
                messages: history,
            };
            // Stream activity frames to the client only when there is a reply
            // channel stream (mirrors deliver_turn_outcome's early return). The sink
            // borrows `relay` for the loop's duration; scope it so the
            // borrow ends before the terminal delivery below reborrows it.
            let result = {
                let mut null_sink = crate::turn::NullSink;
                let mut gateway_sink;
                let sink: &mut dyn StreamSink = if reply_channel_stream.is_some() {
                    gateway_sink = crate::turn::GatewaySink { rpc: &mut *relay };
                    &mut gateway_sink
                } else {
                    &mut null_sink
                };
                agent::llm_loop(
                    max_iterations,
                    &mut *inference,
                    &*tool_router,
                    log,
                    attribution,
                    request,
                    LoopMode {
                        has_client: reply_channel_stream.is_some(),
                        idle_gap,
                        cancel: cancel.clone(),
                        message_id: message_id.clone(),
                    },
                    sink,
                )
                .await
            };
            agent_state.end_turn();
            deliver_turn_outcome(relay, reply_channel_stream.as_deref(), &message_id, &result)
                .await;
            handle_llm_loop_result(result)?;
        }
    };
    tokio::select! {
        res = feeder => res,
        res = turns => res,
    }
}

/// Drain the relay `Subscribe` stream into the harness's queue for as long as
/// the harness runs, so a message that arrives mid-turn is held where a
/// take-back can reach it. Returns only on a source failure.
async fn feed_queue(source: &mut dyn MessageSource, queue: &MessageQueue) -> Result<(), String> {
    loop {
        let inbound = source.next_message().await?;
        queue.push(inbound).await;
    }
}

/// Map an `llm_loop` outcome to the client-facing frame: the assistant reply
/// blocks (present on success / truncated-but-partial) and the terminal
/// turn-state (IDLE on a delivered reply, CANCELLED on a cancel, FAILED
/// otherwise).
fn turn_outcome_frame(
    message_id: &str,
    result: &Result<String, LoopError>,
) -> (Option<Vec<ContentBlock>>, TurnStateEvent) {
    match result {
        Ok(text) | Err(LoopError::Halt(LoopHalt::MaxTokens(text))) => (
            Some(vec![text_block(text.clone())]),
            TurnStateEvent {
                state: TurnState::Idle as i32,
                message_id: message_id.to_string(),
                ..Default::default()
            },
        ),
        // Client-initiated local stop: terminal but not an error — no reply,
        // no failure reason, so the client re-enables input without a banner.
        Err(LoopError::Cancelled) => (
            None,
            TurnStateEvent {
                state: TurnState::Cancelled as i32,
                message_id: message_id.to_string(),
                ..Default::default()
            },
        ),
        Err(e) => (
            None,
            TurnStateEvent {
                state: TurnState::Failed as i32,
                message_id: message_id.to_string(),
                reason: loop_error_reason(e),
                code: "13".into(),
            },
        ),
    }
}

fn loop_error_reason(e: &LoopError) -> String {
    match e {
        LoopError::Halt(LoopHalt::IterationLimit { .. }) => "iteration limit reached".into(),
        LoopError::Halt(LoopHalt::UnknownStop(_)) => "unexpected stop reason".into(),
        LoopError::Halt(LoopHalt::MaxTokens(_)) => "response truncated".into(),
        LoopError::Inference(_) => "model call failed".into(),
        LoopError::StreamEnded(m) => format!("turn stream ended: {m}"),
        LoopError::Cancelled => "turn cancelled".into(),
    }
}

/// Build the harness-emitted turn-start frame. Its WORKING is the agent's turn
/// state for the rest of the turn.
fn turn_start_frame(message_id: &str) -> TurnStateEvent {
    TurnStateEvent {
        state: TurnState::Working as i32,
        message_id: message_id.to_string(),
        ..Default::default()
    }
}

/// Push one turn's outcome to the client via the gateway. The terminal
/// turn-state frame carries the turn's `message_id`; the relay records it as
/// the agent's turn state. No-op when the turn had no reply channel stream;
/// delivery failure is logged, not fatal (the durable log already holds the
/// reply).
async fn deliver_turn_outcome(
    relay: &mut impl RelayRpc,
    reply_channel_stream: Option<&str>,
    message_id: &str,
    result: &Result<String, LoopError>,
) {
    if reply_channel_stream.is_none() {
        tracing::warn!(message_id = %message_id, "no reply channel, dropping turn outcome");
        return;
    }
    let (reply, turn_state) = turn_outcome_frame(message_id, result);
    // Deliver reply first (if any), then turn-state — preserving ordering.
    if let Some(content) = reply {
        tracing::info!(message_id = %message_id, "delivering reply to relay");
        if let Err(e) = relay.deliver_reply(content).await {
            tracing::warn!(error = %e, "failed to deliver reply to client");
        } else {
            tracing::info!(message_id = %message_id, "reply delivered to relay");
        }
    }
    tracing::info!(message_id = %message_id, "delivering turn state to relay");
    if let Err(e) = relay.deliver_turn_state(turn_state).await {
        tracing::warn!(error = %e, "failed to deliver turn state to client");
    } else {
        tracing::info!(message_id = %message_id, "turn state delivered to relay");
    }
}

/// Map an `llm_loop` result to a decision the message loop can act on. `Ok`
/// means wait for the next message. Only `LoopError::Inference` returns `Err`,
/// and the production `InferenceDispatch` never produces it: a failed open
/// arrives as a `TurnError` and ends the turn as `StreamEnded`.
fn handle_llm_loop_result(result: Result<String, LoopError>) -> Result<(), String> {
    match result {
        Ok(_) => Ok(()),
        Err(LoopError::Halt(LoopHalt::IterationLimit { limit })) => {
            tracing::warn!(limit, "iteration limit reached, awaiting next user message");
            Ok(())
        }
        Err(LoopError::Halt(LoopHalt::MaxTokens(partial))) => {
            tracing::warn!(
                partial_bytes = partial.len(),
                "max_tokens reached, awaiting next user message"
            );
            Ok(())
        }
        Err(LoopError::Halt(LoopHalt::UnknownStop(reason))) => {
            tracing::warn!(
                ?reason,
                "unexpected stop reason, awaiting next user message"
            );
            Ok(())
        }
        // The stream ended mid-turn (idle timeout, EOF, a TurnError): the
        // client already has FAILED; await the next message.
        Err(LoopError::StreamEnded(e)) => {
            tracing::warn!(error = %e, "turn ended without completion, awaiting next user message");
            Ok(())
        }
        // A client cancel is a clean per-turn stop, not an infra failure.
        Err(LoopError::Cancelled) => {
            tracing::info!("turn cancelled by client, awaiting next user message");
            Ok(())
        }
        Err(LoopError::Inference(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_doubles::{
        complete_turn, empty_agent_state, test_instructions, test_router, wait_until, write_md,
        TEST_AG,
    };

    #[test]
    fn resolve_primary_agent_missing_file_is_empty_prompt() {
        assert_eq!(resolve_primary_agent(&test_instructions(), TEST_AG), "");
    }

    // The primary turn injects a flat, capped path map from
    // `instructions.list_tree(agent, cap)` into the primary system prompt, keeping
    // AGENTS.md as the required entry prompt.
    #[test]
    fn primary_prompt_injects_flat_instructions_path_map_and_keeps_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ag1/AGENTS.md", "# Primary\n\nEntry prose.");
        write_md(tmp.path(), "ag1/skills/foo/SKILL.md", "s");
        write_md(tmp.path(), "ag1/agents/team/scribe.md", "a");
        let instructions = Instructions::new(tmp.path());

        let agent_text = resolve_primary_agent(&instructions, "ag1");
        let body = strip_frontmatter(&agent_text);
        let prompt = render_primary_prompt(&body, &instructions.list_tree("ag1", LIST_CAP));
        assert!(
            prompt.contains("Entry prose."),
            "AGENTS.md body is kept as the primary prompt: {prompt}"
        );
        assert!(
            prompt.contains("skills/foo/SKILL.md"),
            "the nested skill path is injected into the primary prompt: {prompt}"
        );
        assert!(
            prompt.contains("agents/team/scribe.md"),
            "the nested agent path is injected into the primary prompt: {prompt}"
        );
    }

    #[test]
    fn handle_result_only_propagates_inference_errors() {
        let cases: [(&str, Result<String, LoopError>, Option<&str>); 7] = [
            (
                "iteration limit",
                Err(LoopError::Halt(LoopHalt::IterationLimit { limit: 7 })),
                None,
            ),
            (
                "max tokens with partial text",
                Err(LoopError::Halt(LoopHalt::MaxTokens("partial".into()))),
                None,
            ),
            (
                "unknown stop",
                Err(LoopError::Halt(LoopHalt::UnknownStop(
                    proto_common::StopReason::Unspecified,
                ))),
                None,
            ),
            (
                "inference",
                Err(LoopError::Inference("boom".into())),
                Some("boom"),
            ),
            (
                "stream ended",
                Err(LoopError::StreamEnded("eof".into())),
                None,
            ),
            ("cancelled", Err(LoopError::Cancelled), None),
            ("ok", Ok("final text".into()), None),
        ];
        for (case, result, want_err) in cases {
            let res = handle_llm_loop_result(result);
            assert_eq!(res.err().as_deref(), want_err, "{case}");
        }
    }

    #[test]
    fn a_stream_end_reason_keeps_its_cause() {
        let cause = "turn error 0: model call failed: API error 504 Gateway Timeout";
        let (_, ts) = turn_outcome_frame("c", &Err(LoopError::StreamEnded(cause.into())));
        assert_eq!(ts.reason, format!("turn stream ended: {cause}"));
    }

    #[test]
    fn turn_outcome_only_delivers_a_reply_for_ok_and_max_tokens() {
        // Ok and MaxTokens deliver a reply and land IDLE. The MaxTokens guard is
        // the only branch that splits IDLE from FAILED inside the error space.
        // Every other error — Inference, every non-MaxTokens Halt, and
        // StreamEnded — must land FAILED with no reply, a reason, gRPC status
        // code 13 (INTERNAL) and the turn's message_id (so a client waiting
        // on its own message finds the failure).
        let idle: [Result<String, LoopError>; 2] = [
            Ok("hi".into()),
            Err(LoopError::Halt(LoopHalt::MaxTokens("partial".into()))),
        ];
        for case in &idle {
            let (reply, ts) = turn_outcome_frame("c", case);
            assert!(reply.is_some(), "expected a reply for {case:?}");
            assert_eq!(
                ts.state,
                TurnState::Idle as i32,
                "expected Idle for {case:?}"
            );
            assert_eq!(ts.message_id, "c", "message_id for {case:?}");
        }

        let failed: [Result<String, LoopError>; 4] = [
            Err(LoopError::Inference("boom".into())),
            Err(LoopError::Halt(LoopHalt::IterationLimit { limit: 7 })),
            Err(LoopError::Halt(LoopHalt::UnknownStop(
                proto_common::StopReason::Unspecified,
            ))),
            Err(LoopError::StreamEnded("eof".into())),
        ];
        for case in &failed {
            let (reply, ts) = turn_outcome_frame("c", case);
            assert!(reply.is_none(), "expected no reply for {case:?}");
            assert_eq!(
                ts.state,
                TurnState::Failed as i32,
                "expected Failed for {case:?}"
            );
            assert!(!ts.reason.is_empty(), "expected a reason for {case:?}");
            assert_eq!(ts.message_id, "c", "message_id for {case:?}");
            assert_eq!(ts.code, "13", "INTERNAL status code for {case:?}");
        }

        let (reply, ts) = turn_outcome_frame("c", &Err(LoopError::Cancelled));
        assert_eq!(
            ts.state,
            TurnState::Cancelled as i32,
            "cancel is its own terminal state, not IDLE/FAILED"
        );
        assert_eq!(ts.message_id, "c");
        assert!(
            ts.reason.is_empty(),
            "cancel is terminal-but-not-error; no failure reason"
        );
        assert!(reply.is_none());
    }

    // A one-shot client waits for the terminal state carrying its own
    // message_id, since another person's turn may end first. A terminal frame
    // without the turn's message_id leaves that client waiting forever.
    #[tokio::test]
    async fn deliver_turn_outcome_carries_the_turns_message_id_on_the_terminal_turn_state() {
        let mut relay = WatchedRelay::default();
        let result: Result<String, LoopError> = Ok("done".into());
        deliver_turn_outcome(&mut relay, Some("chan-1"), "mid-1", &result).await;

        let turn_states = relay.turn_states.lock().unwrap();
        assert_eq!(
            turn_states.len(),
            1,
            "one terminal turn-state frame is delivered"
        );
        let turn_state = &turn_states[0];
        assert_eq!(turn_state.state, TurnState::Idle as i32);
        assert_eq!(
            turn_state.message_id, "mid-1",
            "the terminal turn state carries the turn's message_id"
        );
    }

    /// A model whose every turn records the user texts it was sent, then holds
    /// until the test releases one permit, then ends the turn.
    struct GatedTurns {
        started: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        gate: Arc<tokio::sync::Semaphore>,
    }

    struct GatedSource {
        gate: Arc<tokio::sync::Semaphore>,
        events: Option<std::collections::VecDeque<toolset_proto::TurnEvent>>,
    }

    #[async_trait::async_trait]
    impl crate::clients::TurnSource for GatedSource {
        async fn next_event(&mut self) -> Option<Result<toolset_proto::TurnEvent, String>> {
            if self.events.is_none() {
                self.gate.acquire().await.expect("gate stays open").forget();
                self.events =
                    Some(complete_turn(proto_common::StopReason::EndTurn, "ok", &[]).into());
            }
            self.events.as_mut().and_then(|e| e.pop_front()).map(Ok)
        }
    }

    #[async_trait::async_trait]
    impl InferenceRpc for GatedTurns {
        async fn turn(
            &mut self,
            request: TurnRequest,
        ) -> Result<Box<dyn crate::clients::TurnSource>, String> {
            let users = request
                .messages
                .iter()
                .filter(|m| m.role == "user")
                .map(|m| proto_common::content_text(&m.content))
                .collect();
            self.started.lock().unwrap().push(users);
            Ok(Box::new(GatedSource {
                gate: self.gate.clone(),
                events: None,
            }))
        }
    }

    /// The relay's Subscribe stream: hands out what the test sends, counting
    /// each message taken off it, and waits forever once drained.
    struct ChannelSource {
        rx: tokio::sync::mpsc::UnboundedReceiver<crate::message_source::InboundMessage>,
        pulled: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl MessageSource for ChannelSource {
        async fn next_message(&mut self) -> Result<crate::message_source::InboundMessage, String> {
            match self.rx.recv().await {
                Some(m) => {
                    self.pulled
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(m)
                }
                None => std::future::pending().await,
            }
        }
    }

    /// A relay Subscribe stream the test feeds through the returned sender.
    fn channel_source() -> (
        tokio::sync::mpsc::UnboundedSender<crate::message_source::InboundMessage>,
        ChannelSource,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let source = ChannelSource {
            rx,
            pulled: Arc::default(),
        };
        (tx, source)
    }

    /// Run the message loop over `agent_state` with an empty instructions volume.
    async fn run_message_loop(
        inference: &mut GatedTurns,
        relay: &mut WatchedRelay,
        source: &mut ChannelSource,
        agent_state: Arc<AgentState>,
    ) -> Result<(), String> {
        let router = test_router(agent_state.clone());
        message_loop(
            5,
            std::time::Duration::from_secs(30),
            None,
            inference,
            relay,
            &test_instructions(),
            TEST_AG,
            router,
            agent_state,
            source,
        )
        .await
    }

    fn inbound(sender: &str, text: &str) -> crate::message_source::InboundMessage {
        crate::message_source::InboundMessage {
            content: proto_common::text_content(text),
            reply_channel_stream: Some(format!("chan-{sender}")),
            sender: sender.to_string(),
            message_id: format!("mid-{text}"),
        }
    }

    /// A relay whose clones share one record of every turn-state frame the
    /// harness hands it. With `cancel_at_start` set, a turn-start frame
    /// (WORKING) fires a CancelTurn on that agent state, as the CancelTurn
    /// handler does, and records its answer.
    #[derive(Clone, Default)]
    struct WatchedRelay {
        turn_states: Arc<std::sync::Mutex<Vec<TurnStateEvent>>>,
        cancel_at_start: Option<Arc<AgentState>>,
        cancel_answers: Arc<std::sync::Mutex<Vec<bool>>>,
    }

    impl WatchedRelay {
        /// The terminal frames of turns.
        fn terminals(&self) -> Vec<TurnStateEvent> {
            self.turn_states
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.state != TurnState::Working as i32)
                .cloned()
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl RelayRpc for WatchedRelay {
        async fn send_server_notification(
            &mut self,
            _method: &str,
            _params_json: &str,
        ) -> Result<bool, String> {
            Ok(true)
        }
        async fn send_server_request_and_await(
            &mut self,
            _request_id: &str,
            _method: &str,
            _params_json: &str,
            _timeout_seconds: u32,
        ) -> Result<crate::clients::ServerRequestOutcome, String> {
            Ok(crate::clients::ServerRequestOutcome::Result(String::new()))
        }
        async fn deliver_stream_item(
            &mut self,
            _item: proto_common::StreamItem,
        ) -> Result<bool, String> {
            Ok(true)
        }
        async fn deliver_reply(&mut self, _content: Vec<ContentBlock>) -> Result<bool, String> {
            Ok(true)
        }
        async fn deliver_turn_state(&mut self, turn_state: TurnStateEvent) -> Result<bool, String> {
            let starts = turn_state.state == TurnState::Working as i32;
            self.turn_states.lock().unwrap().push(turn_state);
            if let (true, Some(agent_state)) = (starts, &self.cancel_at_start) {
                let answer = agent_state.cancel();
                self.cancel_answers.lock().unwrap().push(answer);
            }
            Ok(true)
        }
    }

    // A message that arrives while idle starts a turn at once. Messages that
    // arrive while that turn runs are taken off the relay stream into the
    // harness's queue and start no turn until it ends; then each starts one
    // turn of its own, oldest first, so each turn adds exactly one message.
    #[tokio::test]
    async fn idle_message_starts_at_once_and_mid_turn_messages_queue_for_one_turn_each_oldest_first(
    ) {
        use std::sync::atomic::Ordering;

        let agent_state = Arc::new(empty_agent_state());
        let started = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let mut inference = GatedTurns {
            started: started.clone(),
            gate: gate.clone(),
        };
        let mut relay = WatchedRelay::default();
        let (tx, mut source) = channel_source();
        let pulled = source.pulled.clone();

        let run = run_message_loop(&mut inference, &mut relay, &mut source, agent_state);
        let starts = || started.lock().unwrap().len();
        let users_of = |turn: usize| started.lock().unwrap()[turn].clone();
        let drive = async {
            tx.send(inbound("alice", "m0")).unwrap();
            wait_until("the idle message starts a turn", || starts() == 1).await;
            assert_eq!(users_of(0), ["m0"]);

            tx.send(inbound("bob", "m1")).unwrap();
            tx.send(inbound("alice", "m2")).unwrap();
            tx.send(inbound("bob", "m3")).unwrap();
            wait_until(
                "the harness takes every mid-turn message off the relay stream",
                || pulled.load(Ordering::SeqCst) == 4,
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert_eq!(
                starts(),
                1,
                "no queued message starts a turn while one runs"
            );

            for n in 1..=3 {
                gate.add_permits(1);
                wait_until("the oldest queued message starts the next turn", || {
                    starts() == n + 1
                })
                .await;
                let want: Vec<String> = (0..=n).map(|i| format!("m{i}")).collect();
                assert_eq!(
                    users_of(n),
                    want,
                    "turn {n} adds exactly the oldest queued message"
                );
            }
        };
        tokio::select! {
            res = run => panic!("the message loop ended: {res:?}"),
            () = drive => {}
        }
    }

    // While a clear or compact holds the turn slot, a message that arrives
    // stays queued: its sender can still take it back, and it never runs.
    #[tokio::test]
    async fn a_message_waiting_behind_a_clear_or_compact_can_still_be_taken_back() {
        use std::sync::atomic::Ordering;

        let agent_state = Arc::new(empty_agent_state());
        let started = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let mut inference = GatedTurns {
            started: started.clone(),
            gate: Arc::new(tokio::sync::Semaphore::new(0)),
        };
        let mut relay = WatchedRelay::default();
        let (tx, mut source) = channel_source();
        let pulled = source.pulled.clone();

        let held = agent_state.clone();
        let slot = held.acquire_turn_slot().await;
        let run = run_message_loop(&mut inference, &mut relay, &mut source, agent_state);
        let starts = || started.lock().unwrap().len();
        let drive = async {
            tx.send(inbound("alice", "m0")).unwrap();
            wait_until("the harness takes the message off the relay stream", || {
                pulled.load(Ordering::SeqCst) == 1
            })
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let taken = held.queue().take_back("alice").await;
            assert_eq!(
                taken.map(|c| proto_common::content_text(&c)).as_deref(),
                Some("m0"),
                "the message waiting behind the clear is still queued"
            );

            drop(slot);
            tx.send(inbound("bob", "m1")).unwrap();
            wait_until("the next message starts a turn", || starts() == 1).await;
            assert_eq!(
                started.lock().unwrap()[0],
                ["m1"],
                "the taken-back message never ran"
            );
        };
        tokio::select! {
            res = run => panic!("the message loop ended: {res:?}"),
            () = drive => {}
        }
    }

    #[tokio::test]
    async fn a_cancel_as_the_turn_start_frame_goes_out_finds_the_turn_and_ends_it_cancelled() {
        let agent_state = Arc::new(empty_agent_state());
        let mut inference = GatedTurns {
            started: Arc::default(),
            gate: Arc::new(tokio::sync::Semaphore::new(0)),
        };
        let mut relay = WatchedRelay {
            cancel_at_start: Some(agent_state.clone()),
            ..Default::default()
        };
        let watched = relay.clone();
        let (tx, mut source) = channel_source();

        let run = run_message_loop(&mut inference, &mut relay, &mut source, agent_state);
        let drive = async {
            tx.send(inbound("alice", "m0")).unwrap();
            wait_until("a cancel is fired as the turn starts", || {
                !watched.cancel_answers.lock().unwrap().is_empty()
            })
            .await;
            assert_eq!(
                *watched.cancel_answers.lock().unwrap(),
                [true],
                "a cancel as the turn-start frame goes out finds the turn"
            );
            wait_until("the turn ends", || watched.terminals().len() == 1).await;
            let ends = watched.terminals();
            let end = &ends[0];
            assert_eq!(end.message_id, "mid-m0");
            assert_eq!(
                end.state,
                TurnState::Cancelled as i32,
                "the cancelled turn ends CANCELLED"
            );
        };
        tokio::select! {
            res = run => panic!("the message loop ended: {res:?}"),
            () = drive => {}
        }
    }

    // Cancelling a turn leaves the queue as it was. The message queued behind
    // the cancelled turn starts the next turn and runs as its own turn.
    #[tokio::test]
    async fn a_message_queued_behind_a_cancelled_turn_starts_the_next_turn() {
        use std::sync::atomic::Ordering;

        let agent_state = Arc::new(empty_agent_state());
        let started = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let mut inference = GatedTurns {
            started: started.clone(),
            gate: gate.clone(),
        };
        let mut relay = WatchedRelay::default();
        let watched = relay.clone();
        let (tx, mut source) = channel_source();
        let pulled = source.pulled.clone();

        let held = agent_state.clone();
        let run = run_message_loop(&mut inference, &mut relay, &mut source, agent_state);
        let starts = || started.lock().unwrap().len();
        let drive = async {
            tx.send(inbound("alice", "m0")).unwrap();
            wait_until("the first message starts a turn", || starts() == 1).await;
            tx.send(inbound("bob", "m1")).unwrap();
            wait_until("the harness queues the mid-turn message", || {
                pulled.load(Ordering::SeqCst) == 2
            })
            .await;

            assert!(held.cancel(), "the cancel finds the running turn");
            wait_until("the cancelled turn ends", || watched.terminals().len() == 1).await;
            let first = &watched.terminals()[0];
            assert_eq!(first.message_id, "mid-m0");
            assert_eq!(first.state, TurnState::Cancelled as i32);

            wait_until(
                "the message queued behind the cancelled turn (mid-m1) starts the next turn",
                || starts() == 2,
            )
            .await;
            assert_eq!(
                started.lock().unwrap()[1].last().map(String::as_str),
                Some("m1"),
                "the next turn adds the queued message"
            );
            gate.add_permits(1);
            wait_until("the queued message's turn ends", || {
                watched.terminals().len() == 2
            })
            .await;
            let second = &watched.terminals()[1];
            assert_eq!(second.message_id, "mid-m1", "m1 runs as its own turn");
            assert_eq!(second.state, TurnState::Idle as i32);
        };
        tokio::select! {
            res = run => panic!("the message loop ended: {res:?}"),
            () = drive => {}
        }
    }
}
