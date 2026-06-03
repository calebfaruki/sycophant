use tightbeam_proto::{
    content_block, ContentBlock, Message, StopReason, TextBlock, TurnRequest, TurnRole,
};

use crate::clients::TightbeamRpc;
use crate::tool_router::ToolDispatcher;
use crate::transponder_tools::{self, LLM_CALL_TOOL_NAME, RECENT_TURNS_TOOL_NAME};
use crate::turn;

pub(crate) fn text_block(text: String) -> ContentBlock {
    ContentBlock {
        block: Some(content_block::Block::Text(TextBlock { text })),
    }
}

/// Which loop is running. The variant carries data that lands on continuation
/// requests and gates which built-in tools are reachable.
pub(crate) enum LoopMode {
    Orchestrator { reply_channel: Option<String> },
    Delegate { correlation_id: String },
}

impl LoopMode {
    fn role(&self) -> Option<i32> {
        match self {
            LoopMode::Orchestrator { .. } => None,
            LoopMode::Delegate { .. } => Some(TurnRole::Delegate as i32),
        }
    }

    fn reply_channel(&self) -> Option<&str> {
        match self {
            LoopMode::Orchestrator { reply_channel } => reply_channel.as_deref(),
            LoopMode::Delegate { .. } => None,
        }
    }

    fn correlation_id(&self) -> Option<&str> {
        match self {
            LoopMode::Orchestrator { .. } => None,
            LoopMode::Delegate { correlation_id } => Some(correlation_id),
        }
    }

    /// Returns `Err(tool_name)` if the mode forbids `llm_call`. A delegate
    /// cannot spawn another delegate.
    fn allow_llm_call(&self) -> Result<(), &'static str> {
        match self {
            LoopMode::Orchestrator { .. } => Ok(()),
            LoopMode::Delegate { .. } => Err("llm_call"),
        }
    }

    /// Returns `Err(tool_name)` if the mode forbids `recent_turns`. A delegate
    /// sees only the system prompt + query the orchestrator hands it, with no
    /// side channel into parent conversation history.
    fn allow_recent_turns(&self) -> Result<(), &'static str> {
        match self {
            LoopMode::Orchestrator { .. } => Ok(()),
            LoopMode::Delegate { .. } => Err("recent_turns"),
        }
    }
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
#[allow(dead_code)] // ToolDispatch reserved for Stage 4/5; tool errors today fold into is_error tool results.
pub(crate) enum LoopError {
    Halt(LoopHalt),
    TightbeamRpc(String),
    ToolDispatch(String),
    /// Delegate-mode loop attempted a forbidden tool. Carries the tool name.
    ForbiddenInDelegate(&'static str),
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

/// Drive an LLM conversation through tool-use cycles until it ends. Used by
/// both the orchestrator (per user message) and the delegate (per `llm_call`).
///
/// `depth` is belt-and-suspenders against the `allow_llm_call` primary guard:
/// orchestrator entry must be depth=0, delegate entry must be depth=1. Any
/// other combination is a delegate-spawning-a-delegate violation and gets
/// rejected before the first turn.
pub(crate) async fn llm_loop(
    max_iterations: u32,
    tightbeam: &mut dyn TightbeamRpc,
    tool_router: &mut dyn ToolDispatcher,
    initial_request: TurnRequest,
    mode: LoopMode,
    depth: u8,
) -> Result<String, LoopError> {
    match (&mode, depth) {
        (LoopMode::Orchestrator { .. }, 0) | (LoopMode::Delegate { .. }, 1) => {}
        _ => return Err(LoopError::ForbiddenInDelegate("nested_loop")),
    }

    let system = initial_request.system.clone();
    let conversation_id = initial_request.conversation_id.clone();
    let reply_channel = mode.reply_channel().map(str::to_string);
    let role = mode.role();
    let correlation_id = mode.correlation_id().map(str::to_string);

    let mut stream = tightbeam
        .turn(initial_request)
        .await
        .map_err(LoopError::TightbeamRpc)?;
    let mut iterations = 0u32;

    loop {
        let result = turn::consume_turn_stream(&mut *stream)
            .await
            .map_err(LoopError::StreamEnded)?;

        match result.stop_reason {
            StopReason::EndTurn => return Ok(collect_text(&result.content)),
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
                    return Ok(collect_text(&result.content));
                }

                let mut tool_result_messages = Vec::with_capacity(result.tool_calls.len());
                for tc in &result.tool_calls {
                    let (output, is_error) = if tc.name == LLM_CALL_TOOL_NAME {
                        if let Err(name) = mode.allow_llm_call() {
                            return Err(LoopError::ForbiddenInDelegate(name));
                        }
                        match transponder_tools::dispatch_llm_call(
                            tightbeam,
                            tool_router,
                            &tc.id,
                            &tc.input_json,
                            max_iterations,
                            depth + 1,
                        )
                        .await
                        {
                            Ok(text) => (text, false),
                            Err(e) => (format!("llm_call error: {e}"), true),
                        }
                    } else if tc.name == RECENT_TURNS_TOOL_NAME {
                        if let Err(name) = mode.allow_recent_turns() {
                            return Err(LoopError::ForbiddenInDelegate(name));
                        }
                        match transponder_tools::dispatch_recent_turns(
                            tightbeam,
                            &conversation_id,
                            &tc.input_json,
                        )
                        .await
                        {
                            Ok(text) => (text, false),
                            Err(e) => (format!("recent_turns error: {e}"), true),
                        }
                    } else {
                        match tool_router.call_tool(&tc.name, &tc.input_json).await {
                            Ok(resp) => (resp.output, resp.is_error),
                            Err(e) => (format!("tool call error: {e}"), true),
                        }
                    };

                    tool_result_messages.push(Message {
                        role: "tool".into(),
                        content: vec![text_block(output)],
                        tool_calls: vec![],
                        tool_call_id: Some(tc.id.clone()),
                        is_error: if is_error { Some(true) } else { None },
                    });
                }

                let continuation = TurnRequest {
                    system: system.clone(),
                    tools: vec![],
                    messages: tool_result_messages,
                    model: None,
                    reply_channel: reply_channel.clone(),
                    role,
                    correlation_id: correlation_id.clone(),
                    conversation_id: conversation_id.clone(),
                };

                stream = tightbeam
                    .turn(continuation)
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
    use tightbeam_proto::{
        turn_event, GetConversationHistoryResponse, ToolCall, ToolDefinition, TurnComplete,
        TurnEvent,
    };

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
        minted: VecDeque<String>,
        history: VecDeque<GetConversationHistoryResponse>,
    }

    impl FakeTightbeam {
        fn new() -> Self {
            Self {
                turns: VecDeque::new(),
                recorded: Vec::new(),
                minted: VecDeque::new(),
                history: VecDeque::new(),
            }
        }
        fn with_turn(mut self, events: Vec<TurnEvent>) -> Self {
            self.turns.push_back(events);
            self
        }
        fn with_conv_id(mut self, id: &str) -> Self {
            self.minted.push_back(id.into());
            self
        }
        fn with_history(mut self, resp: GetConversationHistoryResponse) -> Self {
            self.history.push_back(resp);
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
            self.minted
                .pop_front()
                .ok_or_else(|| "FakeTightbeam: no more conv ids".to_string())
        }

        async fn get_conversation_history(
            &mut self,
            _conversation_id: &str,
            _limit: Option<u32>,
        ) -> Result<GetConversationHistoryResponse, String> {
            self.history
                .pop_front()
                .ok_or_else(|| "FakeTightbeam: no scripted history".to_string())
        }
    }

    struct FakeRouter {
        tools: Vec<ToolDefinition>,
        responses: VecDeque<Result<CallToolResponse, String>>,
    }

    impl FakeRouter {
        fn empty() -> Self {
            Self {
                tools: vec![],
                responses: VecDeque::new(),
            }
        }
        fn with_response(mut self, resp: Result<CallToolResponse, String>) -> Self {
            self.responses.push_back(resp);
            self
        }
    }

    #[async_trait::async_trait]
    impl ToolDispatcher for FakeRouter {
        fn tool_definitions(&self) -> Vec<ToolDefinition> {
            self.tools.clone()
        }
        async fn call_tool(
            &mut self,
            name: &str,
            _input_json: &str,
        ) -> Result<CallToolResponse, String> {
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

    fn orchestrator(reply_channel: Option<&str>) -> LoopMode {
        LoopMode::Orchestrator {
            reply_channel: reply_channel.map(str::to_string),
        }
    }

    fn delegate(correlation_id: &str) -> LoopMode {
        LoopMode::Delegate {
            correlation_id: correlation_id.into(),
        }
    }

    // Row 1: Orch + EndTurn → Ok(final_text)
    #[tokio::test]
    async fn row1_orchestrator_endturn_returns_text() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::EndTurn,
            vec![text_block("hello world".into())],
            vec![],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            orchestrator(None),
            0,
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "hello world"));
    }

    // Row 3: Orch + ToolUse(llm_call) → continuation, role=None
    #[tokio::test]
    async fn row3_orchestrator_dispatches_llm_call_continuation_role_is_none() {
        let mut tb = FakeTightbeam::new()
            // Orchestrator turn 1: ToolUse asking for llm_call
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call(
                    "tc1",
                    LLM_CALL_TOOL_NAME,
                    r#"{"system_prompt":"persona","query":"q"}"#,
                )],
            )])
            // Delegate turn (from dispatch_llm_call): EndTurn
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("delegate result".into())],
                vec![],
            )])
            // Orchestrator continuation: EndTurn
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("final".into())],
                vec![],
            )])
            .with_conv_id("delegate-conv");
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-orch", Some("orch-system")),
            orchestrator(Some("ch-1")),
            0,
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "final"));
        assert_eq!(tb.recorded.len(), 3);
        // Orchestrator continuation
        let cont = &tb.recorded[2];
        assert_eq!(cont.role, None);
        assert_eq!(cont.reply_channel.as_deref(), Some("ch-1"));
        assert_eq!(cont.conversation_id, "conv-orch");
        // Delegate initial request
        let delegate_initial = &tb.recorded[1];
        assert_eq!(delegate_initial.role, Some(TurnRole::Delegate as i32));
        assert_eq!(delegate_initial.correlation_id.as_deref(), Some("tc1"));
        assert_eq!(delegate_initial.conversation_id, "delegate-conv");
    }

    // Row 4: Orch + ToolUse(recent_turns) → continuation, conv_id threaded
    #[tokio::test]
    async fn row4_orchestrator_dispatches_recent_turns_threads_conv_id() {
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", RECENT_TURNS_TOOL_NAME, "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done".into())],
                vec![],
            )])
            .with_history(GetConversationHistoryResponse {
                entries: vec![],
                total_seq: 0,
                truncated: false,
            });
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-99", None),
            orchestrator(None),
            0,
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "done"));
        assert_eq!(tb.recorded[0].conversation_id, "conv-99");
        assert_eq!(tb.recorded[1].conversation_id, "conv-99");
    }

    // Row 5: Orch + ToolUse(router-served) → continuation; is_error propagated
    #[tokio::test]
    async fn row5_orchestrator_router_tool_is_error_propagated() {
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "bash", "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done".into())],
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
            orchestrator(None),
            0,
        )
        .await;
        assert!(matches!(result, Ok(_)));
        let cont = &tb.recorded[1];
        assert_eq!(cont.messages.len(), 1);
        assert_eq!(cont.messages[0].is_error, Some(true));
    }

    // Row 6: Orch + ToolUse with empty tool_calls → Ok(content text)
    #[tokio::test]
    async fn row6_orchestrator_empty_tool_calls_returns_content_text() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::ToolUse,
            vec![text_block("nothing to do".into())],
            vec![],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-1", None),
            orchestrator(None),
            0,
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "nothing to do"));
    }

    // Row 9: Delegate + EndTurn → Ok(text)
    #[tokio::test]
    async fn row9_delegate_endturn_returns_text() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::EndTurn,
            vec![text_block("delegate said this".into())],
            vec![],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-d", None),
            delegate("tc-1"),
            1,
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "delegate said this"));
    }

    // Row 11: Delegate + ToolUse(llm_call) → ForbiddenInDelegate("llm_call")
    #[tokio::test]
    async fn row11_delegate_llm_call_returns_forbidden() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::ToolUse,
            vec![],
            vec![tool_call(
                "tc1",
                LLM_CALL_TOOL_NAME,
                r#"{"system_prompt":"x","query":"y"}"#,
            )],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-d", None),
            delegate("tc-1"),
            1,
        )
        .await;
        assert!(matches!(
            result,
            Err(LoopError::ForbiddenInDelegate("llm_call"))
        ));
    }

    // Row 12: Delegate + ToolUse(recent_turns) → ForbiddenInDelegate("recent_turns")
    #[tokio::test]
    async fn row12_delegate_recent_turns_returns_forbidden() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::ToolUse,
            vec![],
            vec![tool_call("tc1", RECENT_TURNS_TOOL_NAME, "{}")],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-d", None),
            delegate("tc-1"),
            1,
        )
        .await;
        assert!(matches!(
            result,
            Err(LoopError::ForbiddenInDelegate("recent_turns"))
        ));
    }

    // Row 13: Delegate + ToolUse(router-served) → continuation, role=Delegate, correlation_id preserved
    #[tokio::test]
    async fn row13_delegate_router_tool_preserves_role_and_correlation_id() {
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc-call", "bash", "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done".into())],
                vec![],
            )]);
        let mut router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            output: "ok".into(),
            is_error: false,
        }));
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-d", None),
            delegate("tc-outer"),
            1,
        )
        .await;
        assert!(matches!(result, Ok(_)));
        let cont = &tb.recorded[1];
        assert_eq!(cont.role, Some(TurnRole::Delegate as i32));
        assert_eq!(cont.correlation_id.as_deref(), Some("tc-outer"));
    }

    // Row 2: Orch + MaxTokens → Err(Halt::MaxTokens(partial_text))
    #[tokio::test]
    async fn row2_orchestrator_max_tokens_returns_halt_with_partial_text() {
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
            orchestrator(None),
            0,
        )
        .await;
        match result {
            Err(LoopError::Halt(LoopHalt::MaxTokens(text))) => assert_eq!(text, "partial..."),
            other => panic!("expected Halt::MaxTokens with text, got {other:?}"),
        }
    }

    // Row 7: Orch + ToolUse repeated → Err(Halt::IterationLimit)
    #[tokio::test]
    async fn row7_orchestrator_iteration_limit_returns_halt() {
        // max_iterations=2; script 2 turns of router-served ToolUse so the
        // second pass hits the limit.
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "bash", "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc2", "bash", "{}")],
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
            orchestrator(None),
            0,
        )
        .await;
        assert!(matches!(
            result,
            Err(LoopError::Halt(LoopHalt::IterationLimit { limit: 2 }))
        ));
    }

    // Row 8: Orch + UnknownStop → Err(Halt::UnknownStop)
    #[tokio::test]
    async fn row8_orchestrator_unknown_stop_returns_halt() {
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
            orchestrator(None),
            0,
        )
        .await;
        assert!(matches!(
            result,
            Err(LoopError::Halt(LoopHalt::UnknownStop(
                StopReason::Unspecified
            )))
        ));
    }

    // Row 10: Delegate + MaxTokens → Err(Halt::MaxTokens(partial_text))
    #[tokio::test]
    async fn row10_delegate_max_tokens_returns_halt_with_partial_text() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::MaxTokens,
            vec![text_block("delegate partial".into())],
            vec![],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-d", None),
            delegate("tc-1"),
            1,
        )
        .await;
        match result {
            Err(LoopError::Halt(LoopHalt::MaxTokens(text))) => {
                assert_eq!(text, "delegate partial")
            }
            other => panic!("expected Halt::MaxTokens with text, got {other:?}"),
        }
    }

    // Row 15: Delegate + ToolUse repeated → Err(Halt::IterationLimit)
    #[tokio::test]
    async fn row15_delegate_iteration_limit_returns_halt() {
        let mut tb = FakeTightbeam::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "bash", "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc2", "bash", "{}")],
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
            user_request("conv-d", None),
            delegate("tc-1"),
            1,
        )
        .await;
        assert!(matches!(
            result,
            Err(LoopError::Halt(LoopHalt::IterationLimit { limit: 2 }))
        ));
    }

    // Row 16: Delegate + UnknownStop → Err(Halt::UnknownStop)
    #[tokio::test]
    async fn row16_delegate_unknown_stop_returns_halt() {
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
            user_request("conv-d", None),
            delegate("tc-1"),
            1,
        )
        .await;
        assert!(matches!(
            result,
            Err(LoopError::Halt(LoopHalt::UnknownStop(
                StopReason::Unspecified
            )))
        ));
    }

    // Row 14: Delegate + ToolUse with empty tool_calls → Ok(collected text)
    #[tokio::test]
    async fn row14_delegate_empty_tool_calls_returns_content_text() {
        let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
            StopReason::ToolUse,
            vec![text_block("delegate done".into())],
            vec![],
        )]);
        let mut router = FakeRouter::empty();
        let result = llm_loop(
            10,
            &mut tb,
            &mut router,
            user_request("conv-d", None),
            delegate("tc-1"),
            1,
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "delegate done"));
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

    // Stage 5 depth guard: only Orchestrator+0 and Delegate+1 are valid.
    // Any other (mode, depth) is a delegate-spawning-a-delegate violation.
    #[tokio::test]
    async fn depth_guard_rejects_invalid_mode_depth_combinations() {
        let invalid = [
            (orchestrator(None), 1u8),
            (orchestrator(None), 2u8),
            (delegate("tc-1"), 0u8),
            (delegate("tc-1"), 2u8),
        ];
        for (mode, depth) in invalid {
            let mut tb = FakeTightbeam::new(); // no scripted turns — guard fires first
            let mut router = FakeRouter::empty();
            let result = llm_loop(
                10,
                &mut tb,
                &mut router,
                user_request("conv-x", None),
                mode,
                depth,
            )
            .await;
            assert!(
                matches!(result, Err(LoopError::ForbiddenInDelegate("nested_loop"))),
                "depth={depth} should have been rejected; got {result:?}"
            );
        }
    }

    // Row 11 promoted: delegate-mode entry to the llm_call arm never reaches
    // dispatch_llm_call, regardless of conv_id, correlation_id, system prompt,
    // or how many llm_call tool_calls are emitted in one turn. Hand-rolled
    // parameterization instead of pulling in proptest.
    #[tokio::test]
    async fn delegate_llm_call_is_forbidden_across_variants() {
        struct Variant<'a> {
            conv_id: &'a str,
            correlation_id: &'a str,
            system: Option<&'a str>,
            calls: Vec<(&'a str, &'a str)>, // (tool_call_id, input_json)
        }
        let variants = [
            Variant {
                conv_id: "conv-a",
                correlation_id: "tc-1",
                system: None,
                calls: vec![("tc1", r#"{"system_prompt":"x","query":"y"}"#)],
            },
            Variant {
                conv_id: "conv-b",
                correlation_id: "tc-99",
                system: Some("delegate-system"),
                calls: vec![("tc2", r#"{"system_prompt":"a","query":"b"}"#)],
            },
            Variant {
                conv_id: "conv-c",
                correlation_id: "tc-xyz",
                system: Some("another"),
                calls: vec![
                    ("tc3", r#"{"system_prompt":"p","query":"q"}"#),
                    ("tc4", r#"{"system_prompt":"r","query":"s"}"#),
                ],
            },
        ];
        for v in variants {
            let mut tb = FakeTightbeam::new().with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                v.calls
                    .iter()
                    .map(|(id, input)| tool_call(id, LLM_CALL_TOOL_NAME, input))
                    .collect(),
            )]);
            let mut router = FakeRouter::empty();
            let result = llm_loop(
                10,
                &mut tb,
                &mut router,
                user_request(v.conv_id, v.system),
                delegate(v.correlation_id),
                1,
            )
            .await;
            assert!(
                matches!(result, Err(LoopError::ForbiddenInDelegate("llm_call"))),
                "delegate variant should have been rejected; got {result:?}"
            );
            // mint_conversation should never have been called (dispatch_llm_call
            // would have minted one). The fake holds no conv ids, so any attempt
            // to mint would propagate as a Tightbeam error rather than ForbiddenInDelegate.
        }
    }

    #[test]
    fn orchestrator_mode_allows_built_in_tools() {
        let mode = LoopMode::Orchestrator {
            reply_channel: None,
        };
        assert!(mode.allow_llm_call().is_ok());
        assert!(mode.allow_recent_turns().is_ok());
        assert_eq!(mode.role(), None);
        assert_eq!(mode.correlation_id(), None);
    }

    #[test]
    fn delegate_mode_forbids_llm_call_and_recent_turns() {
        let mode = LoopMode::Delegate {
            correlation_id: "tc-1".into(),
        };
        assert_eq!(mode.allow_llm_call().unwrap_err(), "llm_call");
        assert_eq!(mode.allow_recent_turns().unwrap_err(), "recent_turns");
        assert_eq!(mode.role(), Some(TurnRole::Delegate as i32));
        assert_eq!(mode.correlation_id(), Some("tc-1"));
    }
}
