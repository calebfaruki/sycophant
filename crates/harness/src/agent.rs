use proto_common::{content_text, text_block, Message, StopReason};
use tokio::sync::RwLock;
use toolset_proto::TurnRequest;

use crate::clients::InferenceRpc;
use crate::conversation::{AssistantAttribution, ConversationLog};
use crate::runtime_tools::DispatchAbort;
use crate::tool_router::ToolDispatcher;
use crate::turn;

/// Per-call context for one `llm_loop` run.
pub(crate) struct LoopMode {
    pub has_client: bool,
    /// The relay-minted id of the user message this turn answers. Stamped on
    /// every `StreamItem` the turn emits.
    pub message_id: String,
    /// Max silence between inference-job events before the turn is failed as
    /// wedged (vs awaited forever).
    pub idle_gap: std::time::Duration,
    /// Fires on a client `CancelTurn`; the loop abandons the in-flight stream
    /// and returns `LoopError::Cancelled`.
    pub cancel: tokio_util::sync::CancellationToken,
}

/// Why the loop stopped without natural completion.
#[derive(Debug)]
pub(crate) enum LoopHalt {
    IterationLimit {
        limit: u32,
    },
    /// Provider hit the per-response token cap mid-turn. Carries the partial
    /// assistant text so callers can surface it rather than drop.
    MaxTokens(String),
    UnknownStop(StopReason),
}

/// All non-Ok exits from `llm_loop`. `Halt(_)` is "loop stopped before natural
/// completion".
#[derive(Debug)]
pub(crate) enum LoopError {
    Halt(LoopHalt),
    Inference(String),
    StreamEnded(String),
    /// Client-initiated local stop: the turn's cancellation token fired and
    /// the in-flight stream was abandoned. Terminal, but not an error.
    Cancelled,
}

/// Assistant proto message for the result of one upstream turn.
fn assistant_message(result: &turn::TurnResult) -> Message {
    Message {
        role: "assistant".into(),
        content: result.content.clone(),
        tool_calls: result.tool_calls.clone(),
        tool_call_id: None,
        is_error: None,
    }
}

/// Persist an assistant turn to the conversation log. A persist failure is
/// logged, not fatal — an empty assistant turn (no content blocks, no tool
/// calls) is legitimately rejected by the log and simply not stored.
async fn persist_assistant(
    log: &RwLock<ConversationLog>,
    msg: &Message,
    attribution: &AssistantAttribution,
) {
    if let Err(e) = log
        .write()
        .await
        .append_assistant(msg.clone(), attribution.clone())
        .await
    {
        tracing::warn!(error = %e, "skipped persisting assistant turn");
    }
}

/// Drive the agent's LLM conversation through tool-use cycles until
/// it ends. The harness assembles the full message history locally and
/// resends it on every turn (providers are stateless), persisting each
/// assistant turn and tool result to `log` as it goes. `initial_request`
/// already carries the assembled history (incl. the just-appended user
/// message) in its `messages`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn llm_loop(
    max_iterations: u32,
    inference: &mut dyn InferenceRpc,
    tool_router: &dyn ToolDispatcher,
    log: &RwLock<ConversationLog>,
    attribution: AssistantAttribution,
    initial_request: TurnRequest,
    mode: LoopMode,
    sink: &mut dyn turn::StreamSink,
) -> Result<String, LoopError> {
    let idle_gap = mode.idle_gap;
    let cancel = mode.cancel;
    let has_client = mode.has_client;
    // Streamed-item bookkeeping spans every continuation turn in this loop so
    // item ids stay stable.
    let mut emit = turn::EmitState::new(mode.message_id);

    // Running provider context. Seeded with the assembled history; every
    // assistant turn and tool result is appended here (for the next
    // request) AND to the persistent log (for restart continuity).
    let mut request = initial_request;

    let mut stream = {
        tracing::info!("calling model");
        inference
            .turn(request.clone())
            .await
            .map_err(LoopError::Inference)?
    };
    let mut iterations = 0u32;

    loop {
        let result =
            match turn::consume_turn_stream(&mut *stream, idle_gap, sink, &mut emit, &cancel).await
            {
                Ok(result) => result,
                Err(turn::TurnAbort::Ended(e)) => return Err(LoopError::StreamEnded(e)),
                Err(turn::TurnAbort::Cancelled) => return Err(LoopError::Cancelled),
            };

        tracing::info!(stop_reason = ?result.stop_reason, "model responded");

        match result.stop_reason {
            StopReason::EndTurn => {
                let assistant = assistant_message(&result);
                persist_assistant(log, &assistant, &attribution).await;
                return Ok(content_text(&result.content));
            }
            StopReason::MaxTokens => {
                let assistant = assistant_message(&result);
                persist_assistant(log, &assistant, &attribution).await;
                return Err(LoopError::Halt(LoopHalt::MaxTokens(content_text(
                    &result.content,
                ))));
            }
            StopReason::ToolUse => {
                if result.tool_calls.is_empty() {
                    // ToolUse stop reason but no tool calls — treat as an
                    // EndTurn equivalent. Surface the text once rather than
                    // burning iterations on retries.
                    let assistant = assistant_message(&result);
                    persist_assistant(log, &assistant, &attribution).await;
                    return Ok(content_text(&result.content));
                }

                for tc in &result.tool_calls {
                    tracing::info!(tool = %tc.name, "tool call detected");
                }

                iterations += 1;
                if iterations >= max_iterations {
                    return Err(LoopError::Halt(LoopHalt::IterationLimit {
                        limit: max_iterations,
                    }));
                }

                // Persist the assistant tool-use turn before the results so
                // the log (and resent history) stays provider-valid: every
                // tool result is preceded by its assistant tool_use.
                let assistant = assistant_message(&result);
                persist_assistant(log, &assistant, &attribution).await;
                request.messages.push(assistant);

                for tc in &result.tool_calls {
                    let (content, is_error) = match tool_router
                        .call_tool(&tc.name, &tc.input_json, has_client, &tc.id, &cancel)
                        .await
                    {
                        Ok(resp) => (resp.content, resp.is_error),
                        // A cancelled tool call is terminal: hard-exit the whole
                        // turn as Cancelled. It must not fold partial output into
                        // an is_error tool result. But the assistant tool_use was
                        // just persisted, so first give it and every later call of
                        // the message a synthetic matching tool result — an error
                        // carrying only a cancellation marker, no partial output —
                        // keeping the resent history provider-valid (every
                        // tool_use has a paired result).
                        Err(DispatchAbort::Cancelled) => {
                            if let Err(e) = log
                                .write()
                                .await
                                .close_open_calls("tool call cancelled")
                                .await
                            {
                                tracing::warn!(error = %e, "skipped persisting cancelled tool result");
                            }
                            return Err(LoopError::Cancelled);
                        }
                        Err(DispatchAbort::Error(e)) => {
                            (vec![text_block(format!("tool call error: {e}"))], true)
                        }
                    };

                    let tool_msg = Message {
                        role: "tool".into(),
                        content,
                        tool_calls: vec![],
                        tool_call_id: Some(tc.id.clone()),
                        is_error: if is_error { Some(true) } else { None },
                    };
                    if let Err(e) = log.write().await.append(tool_msg.clone()).await {
                        tracing::warn!(error = %e, "skipped persisting tool result");
                    }
                    request.messages.push(tool_msg);
                }

                stream = inference
                    .turn(request.clone())
                    .await
                    .map_err(LoopError::Inference)?;
            }
            other => {
                return Err(LoopError::Halt(LoopHalt::UnknownStop(other)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use proto_common::{content_block, CallToolResponse};
    use toolset_proto::TurnEvent;

    use crate::test_doubles::{
        complete_turn, content_delta, text_delta_of, CapturingSink, ScriptedTurns,
    };

    fn fresh_log() -> RwLock<ConversationLog> {
        RwLock::new(crate::test_doubles::throwaway_log())
    }

    /// Run `llm_loop` with a throwaway log and a `NullSink`.
    async fn run_loop(
        max: u32,
        tb: &mut dyn InferenceRpc,
        router: &dyn ToolDispatcher,
        req: TurnRequest,
        mode: LoopMode,
    ) -> Result<String, LoopError> {
        let log = fresh_log();
        let mut sink = turn::NullSink;
        llm_loop(
            max,
            tb,
            router,
            &log,
            AssistantAttribution::default(),
            req,
            mode,
            &mut sink,
        )
        .await
    }

    struct FakeRouter {
        responses: std::sync::Mutex<VecDeque<Result<CallToolResponse, String>>>,
        /// The last call's tool name and whether a client was connected.
        last_call: std::sync::Mutex<Option<(String, bool)>>,
    }

    impl FakeRouter {
        fn empty() -> Self {
            Self {
                responses: std::sync::Mutex::new(VecDeque::new()),
                last_call: std::sync::Mutex::new(None),
            }
        }
        fn with_response(self, resp: Result<CallToolResponse, String>) -> Self {
            self.responses.lock().unwrap().push_back(resp);
            self
        }
    }

    #[async_trait::async_trait]
    impl ToolDispatcher for FakeRouter {
        async fn call_tool(
            &self,
            name: &str,
            _input_json: &str,
            has_client: bool,
            _tool_call_id: &str,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<CallToolResponse, DispatchAbort> {
            *self.last_call.lock().unwrap() = Some((name.into(), has_client));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(format!("FakeRouter: no scripted response for {name}")))
                .map_err(DispatchAbort::Error)
        }
    }

    fn user_request(system: Option<&str>) -> TurnRequest {
        TurnRequest {
            system: system.map(str::to_string),
            tools: vec![],
            messages: vec![Message {
                role: "user".into(),
                content: vec![text_block("hello".into())],
                tool_calls: vec![],
                tool_call_id: None,
                is_error: None,
            }],
        }
    }

    fn mode(has_client: bool) -> LoopMode {
        LoopMode {
            has_client,
            message_id: "mid-1".into(),
            idle_gap: std::time::Duration::from_secs(45),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn max_tokens_returns_halt_with_partial_text() {
        let mut tb = ScriptedTurns::new(vec![complete_turn(
            StopReason::MaxTokens,
            "partial...",
            &[],
        )]);
        let router = FakeRouter::empty();
        let result = run_loop(10, &mut tb, &router, user_request(None), mode(false)).await;
        match result {
            Err(LoopError::Halt(LoopHalt::MaxTokens(text))) => assert_eq!(text, "partial..."),
            other => panic!("expected Halt::MaxTokens with text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_stop_returns_halt() {
        let mut tb = ScriptedTurns::new(vec![complete_turn(StopReason::Unspecified, "", &[])]);
        let router = FakeRouter::empty();
        let result = run_loop(10, &mut tb, &router, user_request(None), mode(false)).await;
        assert!(matches!(
            result,
            Err(LoopError::Halt(LoopHalt::UnknownStop(
                StopReason::Unspecified
            )))
        ));
    }

    #[tokio::test]
    async fn tool_use_routes_through_router_and_keeps_the_continuation_context() {
        let mut tb = ScriptedTurns::new(vec![
            complete_turn(
                StopReason::ToolUse,
                "",
                &[("tc1", "Bash", r#"{"cmd":"ls"}"#)],
            ),
            complete_turn(StopReason::EndTurn, "done", &[]),
        ]);
        let router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            content: vec![text_block("ls output".into())],
            is_error: false,
        }));
        let result = run_loop(
            10,
            &mut tb,
            &router,
            user_request(Some("orch-system")),
            mode(true),
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "done"));
        let last_call = router.last_call.lock().unwrap().clone().unwrap();
        assert_eq!(last_call.0, "Bash");
        assert!(last_call.1);
        let reqs = tb.requests();
        assert_eq!(reqs[1].system.as_deref(), Some("orch-system"));
    }

    #[tokio::test]
    async fn tool_is_error_propagates_into_continuation_message() {
        let mut tb = ScriptedTurns::new(vec![
            complete_turn(StopReason::ToolUse, "", &[("tc1", "Bash", "{}")]),
            complete_turn(StopReason::EndTurn, "done", &[]),
        ]);
        let router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            content: vec![text_block("tool failed".into())],
            is_error: true,
        }));
        let result = run_loop(10, &mut tb, &router, user_request(None), mode(false)).await;
        assert!(result.is_ok());
        // Full assembled history resent on the continuation: original user
        // turn, the assistant tool_use turn, then the tool result carrying
        // is_error. The error flag must survive onto the tool result.
        let reqs = tb.requests();
        let tool_result = reqs[1]
            .messages
            .last()
            .expect("continuation carries the tool result");
        assert_eq!(tool_result.role, "tool");
        assert_eq!(tool_result.is_error, Some(true));
    }

    // A tool answer of mixed parts (an image followed by a text caption) must
    // fold into the resent conversation history with BOTH parts intact, image
    // first.
    #[tokio::test]
    async fn tool_answer_content_parts_fold_into_conversation_unconverted() {
        let png: Vec<u8> = vec![0x89, 0x50, 0x4e, 0x47, 7, 7, 7];
        let image = proto_common::image_block("image/png".into(), png.clone());
        let mut tb = ScriptedTurns::new(vec![
            complete_turn(StopReason::ToolUse, "", &[("tc1", "preview", "{}")]),
            complete_turn(StopReason::EndTurn, "done", &[]),
        ]);
        let router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            content: vec![image, text_block("caption".into())],
            is_error: false,
        }));
        let result = run_loop(10, &mut tb, &router, user_request(None), mode(false)).await;
        assert!(result.is_ok());

        // The tool result resent on the continuation carries both parts,
        // image first, unchanged.
        let reqs = tb.requests();
        let tool_result = reqs[1]
            .messages
            .last()
            .expect("continuation carries the tool result");
        assert_eq!(tool_result.role, "tool");
        assert_eq!(
            tool_result.content.len(),
            2,
            "both content parts fold through; the answer is not flattened to text"
        );
        match tool_result.content[0].block.as_ref() {
            Some(content_block::Block::Image(img)) => {
                assert_eq!(img.media_type, "image/png");
                assert_eq!(img.data, png, "image bytes fold through untouched");
            }
            other => panic!("expected the image part to survive the fold, got {other:?}"),
        }
    }

    // Providers are stateless, so continuation k resends every tool result
    // folded so far, not only the latest one.
    #[tokio::test]
    async fn each_continuation_resends_every_earlier_tool_result() {
        const N: usize = 3;

        fn tool_results_in(messages: &[Message]) -> usize {
            messages.iter().filter(|m| m.role == "tool").count()
        }

        // N turns that each call `preview` once, then a final EndTurn.
        let mut turns: Vec<Vec<TurnEvent>> = (0..N)
            .map(|i| {
                let id = format!("tc{i}");
                complete_turn(StopReason::ToolUse, "", &[(id.as_str(), "preview", "{}")])
            })
            .collect();
        turns.push(complete_turn(StopReason::EndTurn, "done", &[]));
        let mut tb = ScriptedTurns::new(turns);

        let mut router = FakeRouter::empty();
        for _ in 0..N {
            router = router.with_response(Ok(CallToolResponse {
                content: vec![text_block("ok".into())],
                is_error: false,
            }));
        }

        let result = run_loop(
            (N as u32) + 2,
            &mut tb,
            &router,
            user_request(None),
            mode(false),
        )
        .await;
        assert!(result.is_ok(), "loop should complete: {result:?}");

        let per_request: Vec<usize> = tb
            .requests()
            .iter()
            .map(|r| tool_results_in(&r.messages))
            .collect();
        assert_eq!(
            per_request,
            (0..=N).collect::<Vec<_>>(),
            "request k carries k tool results"
        );
    }

    #[tokio::test]
    async fn iteration_limit_returns_halt() {
        let mut tb = ScriptedTurns::new(vec![
            complete_turn(StopReason::ToolUse, "", &[("tc1", "Bash", "{}")]),
            complete_turn(StopReason::ToolUse, "", &[("tc2", "Bash", "{}")]),
        ]);
        let router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            content: vec![text_block("ok".into())],
            is_error: false,
        }));
        let result = run_loop(2, &mut tb, &router, user_request(None), mode(false)).await;
        assert!(matches!(
            result,
            Err(LoopError::Halt(LoopHalt::IterationLimit { limit: 2 }))
        ));
    }

    #[tokio::test]
    async fn router_err_surfaces_as_tool_result_with_is_error() {
        let mut tb = ScriptedTurns::new(vec![
            complete_turn(StopReason::ToolUse, "", &[("tc1", "Bash", "{}")]),
            complete_turn(StopReason::EndTurn, "done", &[]),
        ]);
        let router = FakeRouter::empty().with_response(Err("toolset down".into()));
        let result = run_loop(10, &mut tb, &router, user_request(None), mode(false)).await;
        assert!(result.is_ok());
        // A router Err folds into an is_error tool result on the resent
        // history, not a loop failure.
        let reqs = tb.requests();
        let tool_result = reqs[1]
            .messages
            .last()
            .expect("continuation carries the tool result");
        assert_eq!(tool_result.role, "tool");
        assert_eq!(tool_result.is_error, Some(true));
    }

    /// EndTurn ends the loop after one model call.
    #[tokio::test]
    async fn endturn_returns_text_in_one_turn() {
        let mut tb = ScriptedTurns::new(vec![complete_turn(StopReason::EndTurn, "hi", &[])]);
        let router = FakeRouter::empty();
        let result = run_loop(10, &mut tb, &router, user_request(None), mode(false)).await;
        assert!(matches!(result, Ok(ref s) if s == "hi"));
        assert_eq!(
            tb.requests().len(),
            1,
            "EndTurn must terminate the loop; no synthetic follow-ups"
        );
    }

    /// ToolUse with empty tool_calls is functionally an EndTurn — the
    /// model produced text but dispatched no tool. Surface the text in
    /// one upstream call rather than retrying.
    #[tokio::test]
    async fn tool_use_empty_calls_returns_text_in_one_turn() {
        let mut tb =
            ScriptedTurns::new(vec![complete_turn(StopReason::ToolUse, "planning...", &[])]);
        let router = FakeRouter::empty();
        let result = run_loop(10, &mut tb, &router, user_request(None), mode(false)).await;
        assert!(matches!(result, Ok(ref s) if s == "planning..."));
        assert_eq!(tb.requests().len(), 1);
    }

    /// A router whose every call is cancelled.
    struct CancellingRouter;

    #[async_trait::async_trait]
    impl ToolDispatcher for CancellingRouter {
        async fn call_tool(
            &self,
            _name: &str,
            _input_json: &str,
            _has_client: bool,
            _tool_call_id: &str,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<CallToolResponse, DispatchAbort> {
            Err(DispatchAbort::Cancelled)
        }
    }

    #[tokio::test]
    async fn cancelled_tool_call_terminates_loop_without_continuing() {
        // The model asks for a tool call; the router reports the call was
        // cancelled. The loop must return LoopError::Cancelled
        // (terminal, mapped downstream to TurnState::Cancelled) and must NOT
        // issue a continuation turn.
        let mut tb = ScriptedTurns::new(vec![
            complete_turn(StopReason::ToolUse, "", &[("tc1", "ShowKernel", "{}")]),
            // A second scripted turn is available ONLY so the forbidden
            // continue-the-loop path has somewhere to go. A correct cancel
            // exit never consumes it.
            complete_turn(StopReason::EndTurn, "resumed", &[]),
        ]);
        let result = run_loop(
            10,
            &mut tb,
            &CancellingRouter,
            user_request(None),
            mode(false),
        )
        .await;

        assert!(
            matches!(result, Err(LoopError::Cancelled)),
            "a cancelled tool call must drive the turn to Cancelled, got {result:?}"
        );
        // No continuation turn was dispatched: exactly the initial turn was
        // sent upstream.
        assert_eq!(
            tb.requests().len(),
            1,
            "cancel must exit before any continuation; no second model turn"
        );
    }

    // The model asks for two calls in one message and the turn is cancelled
    // during the first. Both calls still end in the log, each with exactly one
    // entry: the cancelled one and the one that never started.
    #[tokio::test]
    async fn cancelling_the_first_of_two_calls_gives_each_call_exactly_one_end_entry() {
        let mut tb = ScriptedTurns::new(vec![complete_turn(
            StopReason::ToolUse,
            "",
            &[("tc1", "Bash", "{}"), ("tc2", "Bash", "{}")],
        )]);
        let log = fresh_log();
        let mut sink = turn::NullSink;
        let result = llm_loop(
            10,
            &mut tb,
            &CancellingRouter,
            &log,
            AssistantAttribution::default(),
            user_request(None),
            mode(false),
            &mut sink,
        )
        .await;
        assert!(
            matches!(result, Err(LoopError::Cancelled)),
            "a cancelled tool call is terminal, got {result:?}"
        );

        let history = log.read().await.history_for_provider();
        for id in ["tc1", "tc2"] {
            let ends = history
                .iter()
                .filter(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(id))
                .count();
            assert_eq!(
                ends, 1,
                "call {id} must have exactly one end entry in the log, found {ends}"
            );
        }
    }

    #[tokio::test]
    async fn a_fired_cancel_ends_the_loop_cancelled_with_no_second_model_call() {
        // A turn whose token has fired ends Cancelled and makes no second model
        // call.
        let mut tb = ScriptedTurns::new(vec![vec![]]);
        let router = FakeRouter::empty();
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel(); // fired before the loop consumes the model call

        let result = run_loop(
            10,
            &mut tb,
            &router,
            user_request(None),
            LoopMode {
                cancel,
                ..mode(false)
            },
        )
        .await;

        assert!(
            matches!(result, Err(LoopError::Cancelled)),
            "a fired cancel must end the loop Cancelled, got {result:?}"
        );
        assert_eq!(
            tb.requests().len(),
            1,
            "a cancelled turn appends nothing the loop continues on: no second turn"
        );
    }

    // A tool call cancelled after its assistant `tool_use` was recorded gets a
    // matching tool result written, so the conversation history stays valid for
    // the next turn. That result is an error carrying only a cancellation
    // marker, and it is log-only: output already streamed to the user is not
    // retracted and nothing new is pushed to the client.
    //
    // The result's `tool_call_id` is what pairs it to the dangling `tool_use`.
    #[tokio::test]
    async fn cancelled_tool_call_logs_a_cancelled_error_and_streams_nothing_more() {
        let mut sink = CapturingSink(Vec::new());
        let mut tb = ScriptedTurns::new(vec![[
            vec![content_delta("live-output-text")],
            complete_turn(StopReason::ToolUse, "", &[("tc1", "Bash", "{}")]),
        ]
        .concat()]);
        let router = CancellingRouter;
        let log = fresh_log();
        let result = llm_loop(
            10,
            &mut tb,
            &router,
            &log,
            AssistantAttribution::default(),
            user_request(None),
            mode(false),
            &mut sink,
        )
        .await;
        assert!(
            matches!(result, Err(LoopError::Cancelled)),
            "a cancelled tool call is terminal, got {result:?}"
        );

        let history = log.read().await.history_for_provider();
        let assistant_tool_use = history
            .iter()
            .any(|m| m.role == "assistant" && !m.tool_calls.is_empty());
        assert!(
            assistant_tool_use,
            "the assistant tool_use is persisted before the call (the dangling entry)"
        );
        let tool_result = history
            .iter()
            .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("tc1"))
            .unwrap_or_else(|| {
                panic!(
                    "a matching tool result for tc1 must be persisted so history stays \
                     provider-valid; log held {:?}",
                    history
                        .iter()
                        .map(|m| (m.role.clone(), m.tool_call_id.clone()))
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(
            tool_result.is_error,
            Some(true),
            "a cancelled tool result is marked as an error"
        );
        let blocks = &tool_result.content;
        assert_eq!(
            blocks.len(),
            1,
            "no partial tool output — only the cancellation marker, got {blocks:?}"
        );
        let marker_text = blocks
            .iter()
            .filter_map(|b| match b.block.as_ref() {
                Some(content_block::Block::Text(t)) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<String>()
            .to_lowercase();
        assert!(
            marker_text.contains("cancel"),
            "the sole content block is a cancellation marker, got {marker_text:?}"
        );

        let streamed_live = sink
            .0
            .iter()
            .any(|it| text_delta_of(it) == Some("live-output-text"));
        assert!(
            streamed_live,
            "output streamed before the cancel must remain in the client stream — never retracted"
        );
        assert_eq!(
            sink.0.len(),
            2,
            "the cancel is log-only: only the pre-cancel Start+Delta were emitted, got {}",
            sink.0.len()
        );
        assert!(
            !sink
                .0
                .iter()
                .any(|it| text_delta_of(it).is_some_and(|t| t.to_lowercase().contains("cancel"))),
            "no cancellation notice is pushed to the client display"
        );
    }
}
