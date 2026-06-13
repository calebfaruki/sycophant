use tightbeam_proto::{
    content_block, ContentBlock, Message, StopReason, TextBlock, ToolDefinition, TurnRequest,
};

use crate::clients::TightbeamRpc;
use crate::tool_router::ToolDispatcher;
use crate::turn;

pub(crate) fn text_block(text: String) -> ContentBlock {
    ContentBlock {
        block: Some(content_block::Block::Text(TextBlock { text })),
    }
}

/// Maximum number of synthetic "continue" user messages the loop will
/// inject when the model stops without calling tools and without
/// signalling completion. Sonnet 4.6 sometimes emits a planning sentence
/// and ends the turn even when the persona requires tool calls; the
/// nudge re-enters the loop with a directive to continue or signal
/// completion. Bounded to keep stuck runs from burning unbounded
/// context. Per-message — resets when a new user message arrives.
const MAX_NUDGES: u32 = 5;

/// Sentinel the model must include at the end of its message to signal
/// natural completion. When the model emits `EndTurn` (or `ToolUse` with
/// empty tool_calls) and the text does NOT end with this sentinel, the
/// loop interprets it as an unintended premature stop and sends a nudge.
const DONE_SENTINEL: &str = "DONE";

fn is_done(text: &str) -> bool {
    text.trim_end().ends_with(DONE_SENTINEL)
}

fn nudge_message() -> Message {
    Message {
        role: "user".into(),
        content: vec![text_block(
            "Continue. If the task is fully complete, end your next message with the single \
             word DONE on its own line. Otherwise call the next tool — every assistant turn \
             except the final summary must include at least one tool call. Use the Think tool \
             if you need to record an observation without taking external action."
                .into(),
        )],
        tool_calls: vec![],
        tool_call_id: None,
        is_error: None,
    }
}

/// Per-call context the orchestrator loop needs to stamp on continuation
/// turns. Today only the orchestrator runs through `llm_loop` — sub-agent
/// dispatch is a single round-trip inside `runtime_tools::dispatch_agent`
/// and does NOT re-enter this function.
pub(crate) struct LoopMode {
    pub reply_channel: Option<String>,
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
/// completion"; the other variants are infrastructure failures.
#[derive(Debug)]
#[allow(dead_code)] // ToolDispatch reserved; today tool errors fold into is_error tool results.
pub(crate) enum LoopError {
    Halt(LoopHalt),
    TightbeamRpc(String),
    ToolDispatch(String),
    StreamEnded(String),
}

pub(crate) fn collect_text(content: &[ContentBlock]) -> String {
    let mut buf = String::new();
    for block in content {
        if let Some(content_block::Block::Text(t)) = &block.block {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(&t.text);
        }
    }
    buf
}

/// Drive the orchestrator's LLM conversation through tool-use cycles
/// until it ends. Every tool call routes uniformly through the router;
/// `Runtime` tools (Agent, Agents) dispatch in-process inside the router
/// rather than re-entering this function.
/// Fields that stay constant across every continuation/nudge inside one
/// `llm_loop` invocation. Captured from the initial request, then borrowed
/// by `build_continuation` to compose every subsequent `TurnRequest`.
struct ContinuationCtx {
    system: Option<String>,
    tools: Vec<ToolDefinition>,
    reply_channel: Option<String>,
    conversation_id: String,
}

fn build_continuation(ctx: &ContinuationCtx, messages: Vec<Message>) -> TurnRequest {
    TurnRequest {
        system: ctx.system.clone(),
        tools: ctx.tools.clone(),
        messages,
        model: None,
        reply_channel: ctx.reply_channel.clone(),
        role: None,
        correlation_id: None,
        conversation_id: ctx.conversation_id.clone(),
    }
}

fn build_nudge(ctx: &ContinuationCtx) -> TurnRequest {
    build_continuation(ctx, vec![nudge_message()])
}

pub(crate) async fn llm_loop(
    max_iterations: u32,
    tightbeam: &mut dyn TightbeamRpc,
    tool_router: &mut dyn ToolDispatcher,
    initial_request: TurnRequest,
    mode: LoopMode,
) -> Result<String, LoopError> {
    // Anthropic (and other providers) treat each request as stateless: every
    // POST must carry the full `tools` array, even when continuing an
    // in-progress conversation_id. Capture the unchanging fields into a
    // `ContinuationCtx` once and re-attach via `build_continuation` on every
    // subsequent request — sending an empty `tools` array makes the model
    // return an empty end_turn with no content.
    let ctx = ContinuationCtx {
        system: initial_request.system.clone(),
        tools: initial_request.tools.clone(),
        reply_channel: mode.reply_channel,
        conversation_id: initial_request.conversation_id.clone(),
    };

    let mut stream = tightbeam
        .turn(initial_request)
        .await
        .map_err(LoopError::TightbeamRpc)?;
    let mut iterations = 0u32;
    let mut nudges_used = 0u32;

    loop {
        let result = turn::consume_turn_stream(&mut *stream)
            .await
            .map_err(LoopError::StreamEnded)?;

        match result.stop_reason {
            StopReason::EndTurn => {
                let text = collect_text(&result.content);
                if nudges_used < MAX_NUDGES && !is_done(&text) {
                    nudges_used += 1;
                    tracing::info!(
                        nudges_used,
                        text_bytes = text.len(),
                        "model ended turn without DONE sentinel; nudging to continue"
                    );
                    stream = tightbeam
                        .turn(build_nudge(&ctx))
                        .await
                        .map_err(LoopError::TightbeamRpc)?;
                    continue;
                }
                return Ok(text);
            }
            StopReason::MaxTokens => {
                return Err(LoopError::Halt(LoopHalt::MaxTokens(collect_text(
                    &result.content,
                ))));
            }
            StopReason::ToolUse => {
                iterations += 1;
                if iterations >= max_iterations {
                    return Err(LoopError::Halt(LoopHalt::IterationLimit {
                        limit: max_iterations,
                    }));
                }

                if result.tool_calls.is_empty() {
                    // ToolUse stop reason but no tool calls — same
                    // class of failure as EndTurn-without-tool-calls.
                    // Same nudge response.
                    let text = collect_text(&result.content);
                    if nudges_used < MAX_NUDGES && !is_done(&text) {
                        nudges_used += 1;
                        tracing::info!(
                            nudges_used,
                            text_bytes = text.len(),
                            "model returned ToolUse with empty tool_calls; nudging"
                        );
                        stream = tightbeam
                            .turn(build_nudge(&ctx))
                            .await
                            .map_err(LoopError::TightbeamRpc)?;
                        continue;
                    }
                    return Ok(text);
                }

                let mut tool_result_messages = Vec::with_capacity(result.tool_calls.len());
                for tc in &result.tool_calls {
                    let (output, is_error) = match tool_router
                        .call_tool(&tc.name, &tc.input_json, tightbeam, &ctx.conversation_id)
                        .await
                    {
                        Ok(resp) => (resp.output, resp.is_error),
                        Err(e) => (format!("tool call error: {e}"), true),
                    };

                    tool_result_messages.push(Message {
                        role: "tool".into(),
                        content: vec![text_block(output)],
                        tool_calls: vec![],
                        tool_call_id: Some(tc.id.clone()),
                        is_error: if is_error { Some(true) } else { None },
                    });
                }

                stream = tightbeam
                    .turn(build_continuation(&ctx, tool_result_messages))
                    .await
                    .map_err(LoopError::TightbeamRpc)?;
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

    use airlock_proto::CallToolResponse;
    use tightbeam_proto::{turn_event, ToolCall, TurnComplete, TurnEvent};

    use crate::clients::TurnSource;

    struct FakeTurnSource {
        events: VecDeque<TurnEvent>,
    }

    #[async_trait::async_trait]
    impl TurnSource for FakeTurnSource {
        async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
            self.events.pop_front().map(Ok)
        }
    }

    struct FakeTightbeam {
        turns: VecDeque<Vec<TurnEvent>>,
        recorded: Vec<TurnRequest>,
    }

    impl FakeTightbeam {
        fn new() -> Self {
            Self {
                turns: VecDeque::new(),
                recorded: Vec::new(),
            }
        }
        fn with_turn(mut self, events: Vec<TurnEvent>) -> Self {
            self.turns.push_back(events);
            self
        }
    }

    #[async_trait::async_trait]
    impl TightbeamRpc for FakeTightbeam {
        async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
            self.recorded.push(request);
            let events = self
                .turns
                .pop_front()
                .ok_or_else(|| "FakeTightbeam: no more scripted turns".to_string())?;
            Ok(Box::new(FakeTurnSource {
                events: events.into(),
            }))
        }
        async fn mint_conversation(&mut self) -> Result<String, String> {
            Err("FakeTightbeam: mint_conversation not used in agent.rs tests".into())
        }
    }

    struct FakeRouter {
        responses: VecDeque<Result<CallToolResponse, String>>,
        last_call: Option<(String, String)>,
        last_conv_id: Option<String>,
    }

    impl FakeRouter {
        fn empty() -> Self {
            Self {
                responses: VecDeque::new(),
                last_call: None,
                last_conv_id: None,
            }
        }
        fn with_response(mut self, resp: Result<CallToolResponse, String>) -> Self {
            self.responses.push_back(resp);
            self
        }
    }

    #[async_trait::async_trait]
    impl ToolDispatcher for FakeRouter {
        async fn call_tool(
            &mut self,
            name: &str,
            input_json: &str,
            _tightbeam: &mut dyn TightbeamRpc,
            conversation_id: &str,
        ) -> Result<CallToolResponse, String> {
            self.last_call = Some((name.into(), input_json.into()));
            self.last_conv_id = Some(conversation_id.into());
            self.responses
                .pop_front()
                .unwrap_or_else(|| Err(format!("FakeRouter: no scripted response for {name}")))
        }
    }

    fn complete_event(
        stop: StopReason,
        content: Vec<ContentBlock>,
        calls: Vec<ToolCall>,
    ) -> TurnEvent {
        TurnEvent {
            event: Some(turn_event::Event::Complete(TurnComplete {
                stop_reason: stop as i32,
                content,
                tool_calls: calls,
            })),
        }
    }

    fn tool_call(id: &str, name: &str, input: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            input_json: input.into(),
        }
    }

    fn user_request(conv_id: &str, system: Option<&str>) -> TurnRequest {
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
            model: None,
            reply_channel: None,
            role: None,
            correlation_id: None,
            conversation_id: conv_id.into(),
        }
    }

    fn mode(reply_channel: Option<&str>) -> LoopMode {
        LoopMode {
            reply_channel: reply_channel.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn endturn_returns_text() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::EndTurn,
            vec![text_block("hello world DONE".into())],
            vec![],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "hello world DONE"));
    }

    #[tokio::test]
    async fn max_tokens_returns_halt_with_partial_text() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::MaxTokens,
            vec![text_block("partial...".into())],
            vec![],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        match result {
            Err(LoopError::Halt(LoopHalt::MaxTokens(text))) => assert_eq!(text, "partial..."),
            other => panic!("expected Halt::MaxTokens with text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_stop_returns_halt() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::Unspecified,
            vec![],
            vec![],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(
            result,
            Err(LoopError::Halt(LoopHalt::UnknownStop(
                StopReason::Unspecified
            )))
        ));
    }

    #[tokio::test]
    async fn empty_tool_calls_returns_content_text() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::ToolUse,
            vec![text_block("nothing to do DONE".into())],
            vec![],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "nothing to do DONE"));
    }

    #[tokio::test]
    async fn tool_use_routes_through_router_and_threads_conv_id() {
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "Bash", r#"{"cmd":"ls"}"#)],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done DONE".into())],
                vec![],
            )]);
        let mut router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            output: "ls output".into(),
            is_error: false,
        }));
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-99", Some("orch-system")),
            mode(Some("ch-1")),
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "done DONE"));
        assert_eq!(router.last_call.as_ref().unwrap().0, "Bash");
        assert_eq!(router.last_conv_id.as_deref(), Some("conv-99"));
        // Continuation preserves conversation_id, system, and reply_channel.
        let cont = &tb.recorded[1];
        assert_eq!(cont.conversation_id, "conv-99");
        assert_eq!(cont.system.as_deref(), Some("orch-system"));
        assert_eq!(cont.reply_channel.as_deref(), Some("ch-1"));
        assert_eq!(cont.role, None);
        assert_eq!(cont.correlation_id, None);
    }

    #[tokio::test]
    async fn tool_is_error_propagates_into_continuation_message() {
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "Bash", "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done DONE".into())],
                vec![],
            )]);
        let mut router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            output: "tool failed".into(),
            is_error: true,
        }));
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(result, Ok(_)));
        let cont = &tb.recorded[1];
        assert_eq!(cont.messages.len(), 1);
        assert_eq!(cont.messages[0].is_error, Some(true));
    }

    #[tokio::test]
    async fn iteration_limit_returns_halt() {
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "Bash", "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc2", "Bash", "{}")],
            )]);
        let mut router = FakeRouter::empty()
            .with_response(Ok(CallToolResponse {
                output: "ok".into(),
                is_error: false,
            }))
            .with_response(Ok(CallToolResponse {
                output: "ok".into(),
                is_error: false,
            }));
        let result = llm_loop(
            2,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(
            result,
            Err(LoopError::Halt(LoopHalt::IterationLimit { limit: 2 }))
        ));
    }

    #[tokio::test]
    async fn router_err_surfaces_as_tool_result_with_is_error() {
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "Bash", "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done DONE".into())],
                vec![],
            )]);
        let mut router = FakeRouter::empty().with_response(Err("airlock down".into()));
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(result, Ok(_)));
        let cont = &tb.recorded[1];
        assert_eq!(cont.messages.len(), 1);
        assert_eq!(cont.messages[0].is_error, Some(true));
    }

    #[test]
    fn collect_text_joins_blocks_with_newlines() {
        let blocks = vec![
            text_block("first".to_string()),
            text_block("second".to_string()),
        ];
        assert_eq!(collect_text(&blocks), "first\nsecond");
    }

    #[test]
    fn collect_text_single_block_has_no_separator() {
        let blocks = vec![text_block("only".to_string())];
        assert_eq!(collect_text(&blocks), "only");
    }

    #[test]
    fn collect_text_empty_input_returns_empty_string() {
        assert_eq!(collect_text(&[]), "");
    }

    #[test]
    fn collect_text_skips_non_text_blocks() {
        let blocks = vec![
            text_block("a".to_string()),
            ContentBlock { block: None },
            text_block("b".to_string()),
        ];
        assert_eq!(collect_text(&blocks), "a\nb");
    }

    #[test]
    fn is_done_matches_trailing_sentinel() {
        assert!(is_done("placed all files DONE"));
        assert!(is_done("placed all files\nDONE"));
        assert!(is_done("DONE"));
        assert!(is_done("DONE\n\n"));
        assert!(!is_done("almost done"));
        assert!(!is_done(""));
    }

    /// EndTurn without DONE sentinel should nudge. The fake scripts a
    /// second turn ending in DONE so the loop terminates after one
    /// nudge, and we assert the nudge was sent as a user message.
    #[tokio::test]
    async fn endturn_without_done_nudges_then_returns_on_second_turn() {
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("I'll do the next step.".into())],
                vec![],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("ok finished DONE".into())],
                vec![],
            )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "ok finished DONE"));
        // Two turns sent: original + nudge.
        assert_eq!(tb.recorded.len(), 2);
        // The nudge is a user message — not a tool result.
        let nudge = &tb.recorded[1];
        assert_eq!(nudge.messages.len(), 1);
        assert_eq!(nudge.messages[0].role, "user");
    }

    /// Same nudge behaviour for the ToolUse-with-empty-tool_calls case.
    #[tokio::test]
    async fn tool_use_empty_calls_without_done_nudges() {
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![text_block("planning...".into())],
                vec![],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("ok DONE".into())],
                vec![],
            )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "ok DONE"));
        assert_eq!(tb.recorded.len(), 2);
    }

    /// After MAX_NUDGES retries the loop gives up and returns the last
    /// text — protects against runaway nudging when the model never
    /// recovers.
    #[tokio::test]
    async fn max_nudges_bounds_runaway_no_tool_responses() {
        // MAX_NUDGES + 1 turns, all EndTurn-no-DONE. The loop should
        // nudge MAX_NUDGES times then return the last text.
        let mut tb = FakeTightbeam::new();
        for i in 0..=(MAX_NUDGES as usize) {
            tb = tb.with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block(format!("attempt {i}"))],
                vec![],
            )]);
        }
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            100,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        // Last attempt's text is returned.
        match result {
            Ok(s) => assert!(s.contains("attempt")),
            other => panic!("expected Ok, got {other:?}"),
        }
        // Original turn + MAX_NUDGES nudges == MAX_NUDGES + 1 recorded.
        assert_eq!(tb.recorded.len(), MAX_NUDGES as usize + 1);
    }

    /// EndTurn WITH DONE sentinel must return immediately without
    /// nudging. Prevents a regression where the sentinel check is
    /// accidentally weakened.
    #[tokio::test]
    async fn endturn_with_done_returns_immediately() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::EndTurn,
            vec![text_block("all set DONE".into())],
            vec![],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "all set DONE"));
        // No nudge — exactly one turn.
        assert_eq!(tb.recorded.len(), 1);
    }
}
