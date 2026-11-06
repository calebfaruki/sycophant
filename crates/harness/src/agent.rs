use proto_common::{ContentBlock, Message, StopReason, ToolDefinition};
use tokio::sync::RwLock;
use toolset_proto::TurnRequest;

pub(crate) use proto_common::text_block;

use crate::clients::ToolsetRpc;
use crate::conversation::{AssistantAttribution, ConversationLog};
use crate::runtime_tools::DispatchAbort;
use crate::tool_router::ToolDispatcher;
use crate::turn;

/// Per-call context the agent's loop needs to stamp on continuation turns.
pub(crate) struct LoopMode {
    pub reply_channel_stream: Option<String>,
    /// Max silence between inference-job events before the turn is failed as
    /// wedged (vs awaited forever). Carried here so callers thread it from
    /// config without changing `llm_loop`'s arg list.
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
/// completion"; the other variants are infrastructure failures.
#[derive(Debug)]
#[allow(dead_code)] // ToolDispatch is matched but not yet constructed; reserved for tool dispatch errors.
pub(crate) enum LoopError {
    Halt(LoopHalt),
    ToolsetRpc(String),
    ToolDispatch(String),
    StreamEnded(String),
    /// Client-initiated local stop: the turn's cancellation token fired and
    /// the in-flight stream was abandoned. Terminal, but not an error.
    Cancelled,
}

pub(crate) fn collect_text(content: &[ContentBlock]) -> String {
    proto_common::content_text(content)
}

/// Fields that stay constant across every continuation/nudge inside one
/// `llm_loop` invocation. Captured from the initial request, then borrowed
/// by `build_continuation` to compose every subsequent `TurnRequest`.
struct ContinuationCtx {
    system: Option<String>,
    tools: Vec<ToolDefinition>,
    /// The model the initial request named. A tool-result continuation is the
    /// same logical turn, so it must name the same model: the harness resolves
    /// this against its model catalog and refuses an absent one. There is no
    /// default.
    model: Option<String>,
    reply_channel_stream: Option<String>,
    conversation_id: String,
}

fn build_continuation(ctx: &ContinuationCtx, messages: Vec<Message>) -> TurnRequest {
    TurnRequest {
        system: ctx.system.clone(),
        tools: ctx.tools.clone(),
        messages,
        model: ctx.model.clone(),
        reply_channel_stream: ctx.reply_channel_stream.clone(),
        conversation_id: ctx.conversation_id.clone(),
    }
}

/// Assistant proto message for the result of one upstream turn.
pub(crate) fn assistant_message(result: &turn::TurnResult) -> Message {
    Message {
        role: "assistant".into(),
        content: result.content.clone(),
        tool_calls: result.tool_calls.clone(),
        tool_call_id: None,
        is_error: None,
    }
}

/// Persist an assistant turn to the conversation log. The harness is
/// the sole log author; a persist failure is logged, not fatal — an empty
/// assistant turn (no text, no tool calls) is legitimately rejected by the
/// log and simply not stored.
pub(crate) async fn persist_assistant(
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

/// Drive the orchestrator's LLM conversation through tool-use cycles until
/// it ends. The harness assembles the full message history locally and
/// resends it on every turn (providers are stateless), persisting each
/// assistant turn and tool result to `log` as it goes. `initial_request`
/// already carries the assembled history (incl. the just-appended user
/// message) in its `messages`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn llm_loop(
    max_iterations: u32,
    toolset: &mut dyn ToolsetRpc,
    tool_router: &dyn ToolDispatcher,
    log: &RwLock<ConversationLog>,
    attribution: AssistantAttribution,
    initial_request: TurnRequest,
    mode: LoopMode,
    sink: &mut dyn turn::StreamSink,
    scrub: &shared::scrub::ScrubSet,
) -> Result<String, LoopError> {
    let idle_gap = mode.idle_gap;
    let cancel = mode.cancel;
    // Streamed-item bookkeeping spans every continuation turn in this loop so
    // item ids stay stable.
    let mut emit = turn::EmitState::new(initial_request.conversation_id.clone());
    let ctx = ContinuationCtx {
        system: initial_request.system.clone(),
        tools: initial_request.tools.clone(),
        model: initial_request.model.clone(),
        reply_channel_stream: mode.reply_channel_stream,
        conversation_id: initial_request.conversation_id.clone(),
    };

    // Running provider context. Seeded with the assembled history; every
    // assistant turn and tool result is appended here (for the next
    // request) AND to the persistent log (for restart continuity).
    let mut messages = initial_request.messages.clone();

    let mut stream = {
        tracing::info!(conversation_id = %initial_request.conversation_id, "calling model");
        toolset
            .turn(initial_request)
            .await
            .map_err(LoopError::ToolsetRpc)?
    };
    let mut iterations = 0u32;

    loop {
        let result = match turn::consume_turn_stream_cancellable(
            &mut *stream,
            idle_gap,
            sink,
            &mut emit,
            scrub,
            &cancel,
        )
        .await
        {
            Ok(result) => result,
            Err(turn::TurnAbort::Ended(e)) => return Err(LoopError::StreamEnded(e)),
            Err(turn::TurnAbort::Cancelled) => {
                // The model-call wait was cancelled. Fire exactly one
                // fire-and-forget cancel for this turn's conversation_id,
                // routed through the toolset seam so the in-flight provider
                // call in the llm-job is abandoned. The real client spawns the
                // RPC on a cloned handle, so the turn never blocks on the
                // cancel's completion. Cancelled stays a distinct terminal.
                let _ = toolset.cancel_turn(&ctx.conversation_id).await;
                return Err(LoopError::Cancelled);
            }
        };

        tracing::info!(conversation_id = %ctx.conversation_id, stop_reason = ?result.stop_reason, "model responded");

        match result.stop_reason {
            StopReason::EndTurn => {
                let assistant = assistant_message(&result);
                persist_assistant(log, &assistant, &attribution).await;
                return Ok(collect_text(&result.content));
            }
            StopReason::MaxTokens => {
                let assistant = assistant_message(&result);
                persist_assistant(log, &assistant, &attribution).await;
                return Err(LoopError::Halt(LoopHalt::MaxTokens(collect_text(
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
                    return Ok(collect_text(&result.content));
                }

                for tc in &result.tool_calls {
                    tracing::info!(conversation_id = %ctx.conversation_id, tool = %tc.name, "tool call detected");
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
                messages.push(assistant);

                for tc in &result.tool_calls {
                    let (content, is_error) = match tool_router
                        .call_tool(
                            &tc.name,
                            &tc.input_json,
                            &ctx.conversation_id,
                            ctx.reply_channel_stream.as_deref(),
                            &tc.id,
                            &cancel,
                        )
                        .await
                    {
                        // The answer's content parts fold through as-is — no
                        // conversion into or out of a separate media shape at
                        // this boundary.
                        Ok(resp) => (resp.content, resp.is_error),
                        // A cancelled tool call is terminal: hard-exit the whole
                        // turn as Cancelled. It must not fold partial output into
                        // an is_error tool result. But the assistant tool_use was
                        // just persisted, so first write a synthetic matching tool
                        // result — an error carrying only a cancellation marker,
                        // no partial output — keeping the resent history
                        // provider-valid (every tool_use has a paired result).
                        Err(DispatchAbort::Cancelled) => {
                            let cancelled_msg = Message {
                                role: "tool".into(),
                                content: vec![text_block("tool call cancelled".into())],
                                tool_calls: vec![],
                                tool_call_id: Some(tc.id.clone()),
                                is_error: Some(true),
                            };
                            if let Err(e) = log.write().await.append(cancelled_msg).await {
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
                    messages.push(tool_msg);
                }

                stream = toolset
                    .turn(build_continuation(&ctx, messages.clone()))
                    .await
                    .map_err(LoopError::ToolsetRpc)?;
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

    use proto_common::{content_block, CallToolResponse, ToolCall};
    use toolset_proto::{turn_event, TurnComplete, TurnEvent};

    use crate::clients::TurnSource;
    use crate::test_doubles::ScriptedSource;

    struct FakeToolset {
        turns: VecDeque<Vec<TurnEvent>>,
        recorded: Vec<TurnRequest>,
        /// Every `cancel_turn(conversation_id)` the loop issues, in order.
        /// Shared via `Arc` so the observation survives whether the loop
        /// fires the cancel inline or from a spawned handle. Empty on the
        /// uncancelled path; one entry on the cancelled path.
        cancels: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl FakeToolset {
        fn new() -> Self {
            Self {
                turns: VecDeque::new(),
                recorded: Vec::new(),
                cancels: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
        fn with_turn(mut self, events: Vec<TurnEvent>) -> Self {
            self.turns.push_back(events);
            self
        }
        /// Snapshot of the `cancel_turn` conversation-ids issued so far.
        fn cancels(&self) -> Vec<String> {
            self.cancels.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl ToolsetRpc for FakeToolset {
        async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
            self.recorded.push(request);
            let events = self
                .turns
                .pop_front()
                .ok_or_else(|| "FakeToolset: no more scripted turns".to_string())?;
            Ok(Box::new(ScriptedSource {
                events: events.into(),
            }))
        }

        async fn cancel_turn(&mut self, conversation_id: &str) -> Result<(), String> {
            self.cancels
                .lock()
                .unwrap()
                .push(conversation_id.to_string());
            Ok(())
        }
    }

    fn fresh_log() -> RwLock<ConversationLog> {
        use crate::conversation::LocalFsStore;
        let tmp = tempfile::TempDir::new().unwrap().keep();
        RwLock::new(ConversationLog::new(std::sync::Arc::new(
            LocalFsStore::new(tmp),
        )))
    }

    /// Run `llm_loop` with a throwaway log — the default every test here
    /// uses. Returns the loop result; the log is
    /// internal (tests assert on the loop result and recorded requests).
    async fn run_loop(
        max: u32,
        tb: &mut FakeToolset,
        router: &FakeRouter,
        req: TurnRequest,
        mode: LoopMode,
    ) -> Result<String, LoopError> {
        let log = fresh_log();
        let mut sink = turn::NullSink;
        let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_AGENT_SCRUB__");
        llm_loop(
            max,
            tb,
            router,
            &log,
            AssistantAttribution::default(),
            req,
            mode,
            &mut sink,
            &scrub,
        )
        .await
    }

    struct FakeRouter {
        responses: std::sync::Mutex<VecDeque<Result<CallToolResponse, String>>>,
        last_call: std::sync::Mutex<Option<(String, String)>>,
        last_conv_id: std::sync::Mutex<Option<String>>,
    }

    impl FakeRouter {
        fn empty() -> Self {
            Self {
                responses: std::sync::Mutex::new(VecDeque::new()),
                last_call: std::sync::Mutex::new(None),
                last_conv_id: std::sync::Mutex::new(None),
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
            input_json: &str,
            conversation_id: &str,
            _reply_channel_stream: Option<&str>,
            _tool_call_id: &str,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<CallToolResponse, DispatchAbort> {
            *self.last_call.lock().unwrap() = Some((name.into(), input_json.into()));
            *self.last_conv_id.lock().unwrap() = Some(conversation_id.into());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(format!("FakeRouter: no scripted response for {name}")))
                .map_err(DispatchAbort::Error)
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
            reply_channel_stream: None,
            conversation_id: conv_id.into(),
        }
    }

    fn mode(reply_channel_stream: Option<&str>) -> LoopMode {
        LoopMode {
            reply_channel_stream: reply_channel_stream.map(str::to_string),
            idle_gap: std::time::Duration::from_secs(45),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn max_tokens_returns_halt_with_partial_text() {
        let mut tb = FakeToolset::new().with_turn(vec![complete_event(
            StopReason::MaxTokens,
            vec![text_block("partial...".into())],
            vec![],
        )]);
        let router = FakeRouter::empty();
        let result = run_loop(
            10,
            &mut tb,
            &router,
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
        let mut tb = FakeToolset::new().with_turn(vec![complete_event(
            StopReason::Unspecified,
            vec![],
            vec![],
        )]);
        let router = FakeRouter::empty();
        let result = run_loop(
            10,
            &mut tb,
            &router,
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
    async fn tool_use_routes_through_router_and_threads_conv_id() {
        let mut tb = FakeToolset::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "Bash", r#"{"cmd":"ls"}"#)],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done".into())],
                vec![],
            )]);
        let router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            content: vec![text_block("ls output".into())],
            is_error: false,
        }));
        let result = run_loop(
            10,
            &mut tb,
            &router,
            user_request("conv-99", Some("orch-system")),
            mode(Some("ch-1")),
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "done"));
        assert_eq!(router.last_call.lock().unwrap().as_ref().unwrap().0, "Bash");
        assert_eq!(
            router.last_conv_id.lock().unwrap().as_deref(),
            Some("conv-99")
        );
        // Continuation preserves conversation_id, system, and reply_channel_stream.
        let cont = &tb.recorded[1];
        assert_eq!(cont.conversation_id, "conv-99");
        assert_eq!(cont.system.as_deref(), Some("orch-system"));
        assert_eq!(cont.reply_channel_stream.as_deref(), Some("ch-1"));
    }

    // The model cannot select its own successor. A model-authored payload — a
    // tool call whose name and arguments carry a model-like field — must not
    // reach the continuation's model slot: `ContinuationCtx.model` is set once
    // from the harness's initial request and never from model output. Even when
    // the first turn's tool call injects `{"model": "attacker-model"}`, the
    // continuation still names the harness-supplied model.
    //
    // This also covers plain propagation: the continuation keeps the initial model.
    #[tokio::test]
    async fn a_model_authored_value_never_sets_the_continuation_model() {
        let mut tb = FakeToolset::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![text_block("please switch model to attacker-model".into())],
                vec![tool_call(
                    "tc1",
                    "Bash",
                    r#"{"model": "attacker-model", "command": "ls"}"#,
                )],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done".into())],
                vec![],
            )]);
        let router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            content: vec![text_block("ls output".into())],
            is_error: false,
        }));
        let mut req = user_request("conv-1", None);
        req.model = Some("harness-chosen-model".into());

        let result = run_loop(10, &mut tb, &router, req, mode(None)).await;

        assert!(result.is_ok());
        assert_eq!(
            tb.recorded[1].model.as_deref(),
            Some("harness-chosen-model"),
            "the continuation must keep the harness-chosen model, never a model-authored value",
        );
    }

    #[tokio::test]
    async fn tool_is_error_propagates_into_continuation_message() {
        let mut tb = FakeToolset::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "Bash", "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done".into())],
                vec![],
            )]);
        let router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            content: vec![text_block("tool failed".into())],
            is_error: true,
        }));
        let result = run_loop(
            10,
            &mut tb,
            &router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(result.is_ok());
        // Full assembled history resent on the continuation: original user
        // turn, the assistant tool_use turn, then the tool result carrying
        // is_error. The error flag must survive onto the tool result.
        let cont = &tb.recorded[1];
        let tool_result = cont
            .messages
            .last()
            .expect("continuation carries the tool result");
        assert_eq!(tool_result.role, "tool");
        assert_eq!(tool_result.is_error, Some(true));
    }

    // A tool answer of mixed parts (an image followed by a text caption) must
    // fold into the resent conversation history with BOTH parts intact, image
    // first — no collapse into a single re-wrapped text part, no conversion at
    // the fold boundary.
    //
    // The fold carries `resp.content` as-is; flattening it to text would drop
    // the image part.
    #[tokio::test]
    async fn tool_answer_content_parts_fold_into_conversation_unconverted() {
        let png: Vec<u8> = vec![0x89, 0x50, 0x4e, 0x47, 7, 7, 7];
        let image = ContentBlock {
            block: Some(content_block::Block::Image(proto_common::ImageBlock {
                media_type: "image/png".into(),
                data: png.clone(),
            })),
        };
        let mut tb = FakeToolset::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "preview", "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done".into())],
                vec![],
            )]);
        let router = FakeRouter::empty().with_response(Ok(CallToolResponse {
            content: vec![image, text_block("caption".into())],
            is_error: false,
        }));
        let result = run_loop(
            10,
            &mut tb,
            &router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(result.is_ok());

        // The tool result resent on the continuation carries both parts,
        // image first, unchanged.
        let cont = &tb.recorded[1];
        let tool_result = cont
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

    // Post-mortem receipt for the klein-wenner demo crash: an agent previewing
    // several PDFs in ONE turn re-ships every prior image on every continuation.
    // `llm_loop` pushes each tool result (image bytes and all) onto the running
    // `messages`, then reclones the WHOLE vec onto the next Turn
    // (`build_continuation(&ctx, messages.clone())`). So N previews
    // ship sum(1..=N) images, not N — quadratic in the image count. Against the
    // demo node's 2.35 GiB ceiling, a handful of near-cap PNGs re-shipped each
    // step exhausts memory and takes the single-node cluster down.
    #[tokio::test]
    async fn previewing_several_images_ships_quadratic_bytes() {
        const IMG: usize = 3_670_016; // the 3.5 MiB per-image cap (parts::MAX_IMAGE_BYTES)
        const N: usize = 8; // "several PDFs"

        fn image_block(size: usize) -> ContentBlock {
            ContentBlock {
                block: Some(content_block::Block::Image(proto_common::ImageBlock {
                    media_type: "image/png".into(),
                    data: vec![0u8; size],
                })),
            }
        }
        fn image_bytes_in(messages: &[Message]) -> usize {
            messages
                .iter()
                .flat_map(|m| &m.content)
                .filter_map(|b| match b.block.as_ref() {
                    Some(content_block::Block::Image(img)) => Some(img.data.len()),
                    _ => None,
                })
                .sum()
        }

        // N turns that each call `preview` once, then a final EndTurn.
        let mut tb = FakeToolset::new();
        for i in 0..N {
            tb = tb.with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call(&format!("tc{i}"), "preview", "{}")],
            )]);
        }
        tb = tb.with_turn(vec![complete_event(
            StopReason::EndTurn,
            vec![text_block("done".into())],
            vec![],
        )]);

        // Each preview returns one near-cap image.
        let mut router = FakeRouter::empty();
        for _ in 0..N {
            router = router.with_response(Ok(CallToolResponse {
                content: vec![image_block(IMG)],
                is_error: false,
            }));
        }

        let result = run_loop(
            (N as u32) + 2,
            &mut tb,
            &router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(result.is_ok(), "loop should complete: {result:?}");

        // Receipts. The N previews produce N distinct images. But request k
        // carries every image folded so far, so the loop ships sum(0..=N) =
        // N(N+1)/2 images across its continuations.
        let per_request: Vec<usize> = tb
            .recorded
            .iter()
            .map(|r| image_bytes_in(&r.messages))
            .collect();
        let cumulative: usize = per_request.iter().sum();
        let peak: usize = per_request.iter().copied().max().unwrap();
        let distinct = N * IMG;
        let expected_cumulative = (N * (N + 1) / 2) * IMG;

        eprintln!(
            "RECEIPT previews={N} img_cap={IMG}B  distinct={} MiB  peak_turn={} MiB  \
             cumulative_shipped={} MiB  amplification={:.1}x  per_request_MiB={:?}",
            distinct >> 20,
            peak >> 20,
            cumulative >> 20,
            cumulative as f64 / distinct as f64,
            per_request.iter().map(|b| b >> 20).collect::<Vec<_>>(),
        );

        assert_eq!(
            cumulative, expected_cumulative,
            "the history reclone ships sum(1..=N) images, not N — quadratic in image count"
        );
        assert_eq!(
            peak, distinct,
            "the final continuation Turn alone re-ships all N images in one message vec"
        );
        assert!(
            cumulative >= distinct * 4,
            "8 near-cap previews ship 4.5x their distinct image data in one turn; \
             unbounded in turn length"
        );
    }

    #[tokio::test]
    async fn iteration_limit_returns_halt() {
        let mut tb = FakeToolset::new()
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
        let router = FakeRouter::empty()
            .with_response(Ok(CallToolResponse {
                content: vec![text_block("ok".into())],
                is_error: false,
            }))
            .with_response(Ok(CallToolResponse {
                content: vec![text_block("ok".into())],
                is_error: false,
            }));
        let result = run_loop(
            2,
            &mut tb,
            &router,
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
        let mut tb = FakeToolset::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "Bash", "{}")],
            )])
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("done".into())],
                vec![],
            )]);
        let router = FakeRouter::empty().with_response(Err("toolset down".into()));
        let result = run_loop(
            10,
            &mut tb,
            &router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(result.is_ok());
        // A router Err folds into an is_error tool result on the resent
        // history, not a loop failure.
        let cont = &tb.recorded[1];
        let tool_result = cont
            .messages
            .last()
            .expect("continuation carries the tool result");
        assert_eq!(tool_result.role, "tool");
        assert_eq!(tool_result.is_error, Some(true));
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

    /// Conversational reply: model returns text with EndTurn, loop
    /// returns immediately in exactly one upstream call. Regression
    /// guard for the "hello → 6 bubbles" bug where the framework used
    /// to nudge after every EndTurn that lacked a DONE sentinel.
    #[tokio::test]
    async fn endturn_returns_text_in_one_turn() {
        let mut tb = FakeToolset::new().with_turn(vec![complete_event(
            StopReason::EndTurn,
            vec![text_block("hi".into())],
            vec![],
        )]);
        let router = FakeRouter::empty();
        let result = run_loop(
            10,
            &mut tb,
            &router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "hi"));
        assert_eq!(
            tb.recorded.len(),
            1,
            "EndTurn must terminate the loop; no synthetic follow-ups"
        );
    }

    /// ToolUse with empty tool_calls is functionally an EndTurn — the
    /// model produced text but dispatched no tool. Surface the text in
    /// one upstream call rather than retrying.
    #[tokio::test]
    async fn tool_use_empty_calls_returns_text_in_one_turn() {
        let mut tb = FakeToolset::new().with_turn(vec![complete_event(
            StopReason::ToolUse,
            vec![text_block("planning...".into())],
            vec![],
        )]);
        let router = FakeRouter::empty();
        let result = run_loop(
            10,
            &mut tb,
            &router,
            user_request("conv-1", None),
            mode(None),
        )
        .await;
        assert!(matches!(result, Ok(ref s) if s == "planning..."));
        assert_eq!(tb.recorded.len(), 1);
    }

    // A cancelled tool call surfaces as the distinct `DispatchAbort::Cancelled`
    // carrier (NOT an is_error tool result and NOT the Err(String) channel —
    // both of those continue the loop). `llm_loop` must hard-exit with
    // `LoopError::Cancelled` on that carrier, BEFORE building/persisting/pushing
    // any tool result: the parent turn terminates in the Cancelled state without
    // appending a loop-continuing tool result.

    use crate::runtime_tools::DispatchAbort;

    /// A router whose single scripted response is a cancelled tool call. It
    /// records whether it was called, so the test can prove the cancel was
    /// observed exactly once and no tool-result continuation followed.
    struct CancellingRouter;

    #[async_trait::async_trait]
    impl ToolDispatcher for CancellingRouter {
        async fn call_tool(
            &self,
            _name: &str,
            _input_json: &str,
            _conversation_id: &str,
            _reply_channel_stream: Option<&str>,
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
        // append a tool result / issue a continuation turn.
        let mut tb = FakeToolset::new()
            .with_turn(vec![complete_event(
                StopReason::ToolUse,
                vec![],
                vec![tool_call("tc1", "ShowKernel", "{}")],
            )])
            // A second scripted turn is available ONLY so the forbidden
            // continue-the-loop path has somewhere to go. A correct cancel
            // exit never consumes it.
            .with_turn(vec![complete_event(
                StopReason::EndTurn,
                vec![text_block("resumed".into())],
                vec![],
            )]);
        // Call `llm_loop` directly (the shared `run_loop` helper is pinned to
        // `&FakeRouter`); this test supplies its own cancelling dispatcher.
        let router = CancellingRouter;
        let log = fresh_log();
        let mut sink = turn::NullSink;
        let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_AGENT_SCRUB__");
        let result = llm_loop(
            10,
            &mut tb,
            &router,
            &log,
            AssistantAttribution::default(),
            user_request("conv-1", None),
            mode(None),
            &mut sink,
            &scrub,
        )
        .await;

        assert!(
            matches!(result, Err(LoopError::Cancelled)),
            "a cancelled tool call must drive the turn to Cancelled, got {result:?}"
        );
        // No tool result was appended, so no continuation turn was dispatched:
        // exactly the initial turn was sent upstream.
        assert_eq!(
            tb.recorded.len(),
            1,
            "cancel must exit before any continuation; no second toolset turn"
        );
    }

    /// A turn source whose `next_event` PARKS forever. Only a fired cancel can
    /// end a consume of it; a drained/loop consume hangs.
    struct ParkedSource;
    #[async_trait::async_trait]
    impl TurnSource for ParkedSource {
        async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
            std::future::pending().await
        }
    }

    /// A toolset whose `turn` hands back a never-completing PARKED source, and
    /// records the requests it received. Used to prove the model-call wait is
    /// unblocked by a fired cancel rather than left awaiting forever.
    struct ParkedToolset {
        recorded: Vec<TurnRequest>,
    }
    #[async_trait::async_trait]
    impl ToolsetRpc for ParkedToolset {
        async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
            self.recorded.push(request);
            Ok(Box::new(ParkedSource))
        }
        async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn cancelled_mode() -> LoopMode {
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel(); // fired before the loop consumes the model call
        LoopMode {
            reply_channel_stream: None,
            idle_gap: std::time::Duration::from_secs(45),
            cancel,
        }
    }

    #[tokio::test]
    async fn cancel_fires_exactly_one_cancel_turn_carrying_conversation_id() {
        // The token is fired before the loop consumes the model call, so the
        // biased cancel arm in `consume_turn_stream_cancellable` wins and the
        // loop takes its Cancelled exit. On that exit it must issue exactly one
        // `cancel_turn` to the toolset, carrying THIS turn's conversation_id.
        let mut tb = FakeToolset::new().with_turn(vec![complete_event(
            StopReason::EndTurn,
            vec![text_block(
                "scripted; never consumed — the fired token wins".into(),
            )],
            vec![],
        )]);
        let router = FakeRouter::empty();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            run_loop(
                10,
                &mut tb,
                &router,
                user_request("conv-77", None),
                cancelled_mode(),
            ),
        )
        .await
        .expect("llm_loop must return promptly on cancel, not deadlock on the cancel RPC");

        assert!(
            matches!(result, Err(LoopError::Cancelled)),
            "a fired cancel drives the turn to Cancelled, got {result:?}"
        );

        // The fire-and-forget cancel may land just after the loop returns;
        // give it a bounded moment before asserting, then require exactly one.
        let mut fired = tb.cancels();
        for _ in 0..50 {
            if !fired.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            fired = tb.cancels();
        }
        assert_eq!(
            fired,
            vec!["conv-77".to_string()],
            "exactly one cancel_turn, carrying the turn's conversation_id"
        );
    }

    #[tokio::test]
    async fn cancelled_model_call_unblocks_parked_wait_and_appends_no_continuation() {
        // The toolset hands back a source whose `next_event` never resolves:
        // the harness is genuinely PARKED awaiting the model call. A fired
        // cancel must unblock that park with the terminal Cancelled outcome —
        // not hang, and not feed a result back that dispatches another turn.
        let mut tb = ParkedToolset {
            recorded: Vec::new(),
        };
        let router = FakeRouter::empty();
        let log = fresh_log();
        let mut sink = turn::NullSink;
        let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_AGENT_SCRUB__");

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            llm_loop(
                10,
                &mut tb,
                &router,
                &log,
                AssistantAttribution::default(),
                user_request("conv-parked", None),
                cancelled_mode(),
                &mut sink,
                &scrub,
            ),
        )
        .await
        .expect("a fired cancel must unblock the PARKED model-call wait; hanging is the failure");

        assert!(
            matches!(result, Err(LoopError::Cancelled)),
            "the parked wait must be unblocked with terminal Cancelled, got {result:?}"
        );
        assert_eq!(
            tb.recorded.len(),
            1,
            "a cancelled turn appends nothing the orchestrator continues on: no second turn"
        );
    }

    #[tokio::test]
    async fn uncancelled_model_call_returns_result_and_fires_no_cancel() {
        // Happy path: the model completes with EndTurn and the cancel token is
        // never fired. The result must come back verbatim AND the new cancel
        // machinery must stay silent.
        let mut tb = FakeToolset::new().with_turn(vec![complete_event(
            StopReason::EndTurn,
            vec![text_block("hello world".into())],
            vec![],
        )]);
        let router = FakeRouter::empty();

        let result = run_loop(
            10,
            &mut tb,
            &router,
            user_request("conv-ok", None),
            mode(None),
        )
        .await;

        assert!(
            matches!(result, Ok(ref s) if s == "hello world"),
            "uncancelled turn returns its result unchanged, got {result:?}"
        );
        assert!(
            tb.cancels().is_empty(),
            "no cancellation fired, so the turn must issue no cancel_turn"
        );
    }

    // The cancelled tool-result fix.
    //
    // A toolset tool call cancelled mid-run surfaces as `DispatchAbort::Cancelled`
    // at the tool-execution site (agent.rs Cancelled arm). Today that arm returns
    // immediately, leaving the assistant `tool_use` (persisted just before the
    // call) with no matching tool result — the resent history is provider-invalid
    // on the next turn. The fix persists a synthetic, error-marked, marker-only
    // tool result for `tc.id` before returning Cancelled.

    /// One streamed assistant text delta, so the client sink captures live
    /// output before the tool call is reached.
    fn content_delta_event(text: &str) -> TurnEvent {
        TurnEvent {
            event: Some(turn_event::Event::ContentDelta(
                toolset_proto::ContentDelta { text: text.into() },
            )),
        }
    }

    /// A sink that records every streamed item, so a test can prove the cancel
    /// path neither retracts nor appends client-facing output.
    struct CapturingSink(Vec<proto_common::StreamItem>);

    #[async_trait::async_trait]
    impl turn::StreamSink for CapturingSink {
        async fn emit(&mut self, item: proto_common::StreamItem) {
            self.0.push(item);
        }
    }

    fn text_delta_of(item: &proto_common::StreamItem) -> Option<&str> {
        match item.phase.as_ref()? {
            proto_common::stream_item::Phase::Delta(d) => match d.kind.as_ref()? {
                proto_common::item_delta::Kind::TextDelta(s) => Some(s.as_str()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Drive one ToolUse turn whose single tool call is reported cancelled by
    /// the router, capturing the conversation log and the client sink.
    async fn run_cancelled_tool_call(
        turn_events: Vec<TurnEvent>,
        sink: &mut CapturingSink,
    ) -> (Result<String, LoopError>, RwLock<ConversationLog>) {
        let mut tb = FakeToolset::new().with_turn(turn_events);
        let router = CancellingRouter;
        let log = fresh_log();
        let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_AGENT_SCRUB__");
        let result = llm_loop(
            10,
            &mut tb,
            &router,
            &log,
            AssistantAttribution::default(),
            user_request("conv-1", None),
            mode(None),
            sink,
            &scrub,
        )
        .await;
        (result, log)
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
        let (result, log) = run_cancelled_tool_call(
            vec![
                content_delta_event("live-output-text"),
                complete_event(
                    StopReason::ToolUse,
                    vec![],
                    vec![tool_call("tc1", "Bash", "{}")],
                ),
            ],
            &mut sink,
        )
        .await;
        assert!(
            matches!(result, Err(LoopError::Cancelled)),
            "a cancelled tool call is terminal, got {result:?}"
        );

        let history = log.read().await.history();
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
