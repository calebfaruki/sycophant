use std::time::Duration;

use proto_common::{
    item_delta, item_start, stream_item, ItemDelta, ItemStart, ItemStop, StreamItem, TextItem,
    ToolUseItem,
};
use proto_common::{ContentBlock, StopReason, ToolCall};
use toolset_proto::{turn_event, TurnComplete, TurnEvent};

use crate::clients::{RelayRpc, TurnSource};

/// Sink for streamed activity frames produced during a turn. A turn with a
/// reply channel stream backs it with a gateway RPC; one without, a no-op.
#[async_trait::async_trait]
pub(crate) trait StreamSink: Send {
    async fn emit(&mut self, item: StreamItem);
}

/// A sink that drops every frame. Used where streamed items are absent (no
/// reply channel stream).
pub(crate) struct NullSink;

#[async_trait::async_trait]
impl StreamSink for NullSink {
    async fn emit(&mut self, _item: StreamItem) {}
}

/// Streamed-activity sink backed by the gateway's `DeliverStreamItem` RPC.
/// Delivery is best-effort (a failure is logged, then dropped) — a dropped
/// delta must never fail the turn.
pub(crate) struct GatewaySink<'a> {
    pub(crate) rpc: &'a mut dyn RelayRpc,
    pub(crate) channel_stream_id: String,
}

#[async_trait::async_trait]
impl StreamSink for GatewaySink<'_> {
    async fn emit(&mut self, item: StreamItem) {
        if let Err(e) = self
            .rpc
            .deliver_stream_item(&self.channel_stream_id, item)
            .await
        {
            tracing::warn!(error = %e, "failed to deliver streamed item");
        }
    }
}

/// Per-turn emit bookkeeping: which item runs are currently open. Threaded
/// across every `consume_turn_stream` call within one `llm_loop` so item ids
/// stay stable across continuation turns.
pub(crate) struct EmitState {
    pub conversation_id: String,
    /// Item id of the currently-open text run, if any.
    text_item: Option<String>,
    /// Item id of the currently-open tool-use run (the upstream tool id).
    tool_item: Option<String>,
    /// Counter minting stable text-run item ids (upstream sends no id for text).
    text_runs: u64,
}

impl EmitState {
    pub(crate) fn new(conversation_id: String) -> Self {
        Self {
            conversation_id,
            text_item: None,
            tool_item: None,
            text_runs: 0,
        }
    }

    fn next_frame(&mut self, item_id: String, phase: stream_item::Phase) -> StreamItem {
        StreamItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            item_id,
            conversation_id: self.conversation_id.clone(),
            phase: Some(phase),
        }
    }
}

/// Map one toolset turn-event to the streamed-item frames it produces. Pure
/// aside from the `state` bookkeeping it advances (open-run ids).
/// Terminal `Complete`/`Error` and other events produce no frames — the
/// terminal reply is delivered separately.
pub(crate) fn stream_items_for(event: &TurnEvent, state: &mut EmitState) -> Vec<StreamItem> {
    match &event.event {
        // Streamed assistant text. Empty deltas are provider heartbeats — skip
        // them so an empty run never opens a text item.
        Some(turn_event::Event::ContentDelta(d)) if !d.text.is_empty() => {
            let mut frames = Vec::new();
            let item_id = match &state.text_item {
                Some(id) => id.clone(),
                None => {
                    state.text_runs += 1;
                    let id = format!("text-{}", state.text_runs);
                    state.text_item = Some(id.clone());
                    frames.push(state.next_frame(
                        id.clone(),
                        stream_item::Phase::Start(ItemStart {
                            kind: Some(item_start::Kind::Text(TextItem {})),
                        }),
                    ));
                    id
                }
            };
            frames.push(state.next_frame(
                item_id,
                stream_item::Phase::Delta(ItemDelta {
                    kind: Some(item_delta::Kind::TextDelta(d.text.clone())),
                }),
            ));
            frames
        }
        // A new tool call. Ends any open text run and opens a tool item keyed
        // by the upstream tool id.
        Some(turn_event::Event::ToolUseStart(t)) => {
            let mut frames = Vec::new();
            if let Some(id) = state.text_item.take() {
                frames.push(state.next_frame(id, stream_item::Phase::Stop(ItemStop {})));
            }
            state.tool_item = Some(t.id.clone());
            frames.push(state.next_frame(
                t.id.clone(),
                stream_item::Phase::Start(ItemStart {
                    kind: Some(item_start::Kind::ToolUse(ToolUseItem {
                        name: t.name.clone(),
                    })),
                }),
            ));
            frames
        }
        // Partial JSON arguments for the open tool item.
        Some(turn_event::Event::ToolUseInput(i)) => match &state.tool_item {
            Some(id) => vec![state.next_frame(
                id.clone(),
                stream_item::Phase::Delta(ItemDelta {
                    kind: Some(item_delta::Kind::ToolInputJson(i.partial_json.clone())),
                }),
            )],
            None => vec![],
        },
        _ => vec![],
    }
}

#[derive(Debug)]
pub(crate) struct TurnResult {
    pub stop_reason: StopReason,
    pub content: Vec<ContentBlock>,
    pub tool_calls: Vec<ToolCall>,
}

/// Why a turn stream stopped without a terminal `Complete`. `Ended` folds the
/// existing string errors (idle timeout, EOF, stream error, TurnError);
/// `Cancelled` is a client-initiated local stop that abandons the stream.
#[derive(Debug)]
pub(crate) enum TurnAbort {
    Ended(String),
    Cancelled,
}

/// Like [`consume_turn_stream`] but races the turn against a cancellation
/// token. When the token fires, the toolset stream is abandoned (dropped by
/// the caller when this returns) and `Cancelled` is returned WITHOUT draining
/// the remaining events — this is the "abandon in-flight work" half of a
/// local stop.
pub(crate) async fn consume_turn_stream_cancellable(
    source: &mut dyn TurnSource,
    idle_gap: Duration,
    sink: &mut dyn StreamSink,
    emit: &mut EmitState,
    scrub: &shared::scrub::ScrubSet,
    token: &tokio_util::sync::CancellationToken,
) -> Result<TurnResult, TurnAbort> {
    loop {
        let event = tokio::select! {
            biased;
            _ = token.cancelled() => return Err(TurnAbort::Cancelled),
            r = tokio::time::timeout(idle_gap, source.next_event()) => r,
        };
        match event {
            Err(_) => {
                return Err(TurnAbort::Ended(format!(
                    "idle timeout: no inference-job progress in {}s",
                    idle_gap.as_secs()
                )))
            }
            Ok(None) => return Err(TurnAbort::Ended("stream ended without TurnComplete".into())),
            Ok(Some(event)) => {
                let event = event.map_err(TurnAbort::Ended)?;
                // Streamed activity frames emit DURING the turn, before the
                // terminal Complete/Error is returned to the caller.
                for mut frame in stream_items_for(&event, emit) {
                    scrub_frame(scrub, &mut frame);
                    sink.emit(frame).await;
                }
                if let Some(result) = process_turn_event(event).map_err(TurnAbort::Ended)? {
                    return Ok(result);
                }
            }
        }
    }
}

/// Apply secret-scrubbing to a streamed frame's tool-use content before it
/// crosses the gRPC boundary — the tool name on an `ItemStart` and the
/// partial-JSON arguments on an `ItemDelta`. Text frames carry model prose,
/// not secrets, so they pass through untouched.
// scrub seam wired; no-op until harness holds a secret registry
fn scrub_frame(scrub: &shared::scrub::ScrubSet, frame: &mut StreamItem) {
    if scrub.is_empty() {
        return;
    }
    match &mut frame.phase {
        Some(stream_item::Phase::Start(ItemStart {
            kind: Some(item_start::Kind::ToolUse(t)),
        })) => {
            t.name = scrub.apply(&t.name);
        }
        Some(stream_item::Phase::Delta(ItemDelta {
            kind: Some(item_delta::Kind::ToolInputJson(j)),
        })) => {
            *j = scrub.apply(j);
        }
        _ => {}
    }
}

/// Pure event-processing logic, separated for testability.
///
/// Returns `Ok(Some(result))` on a terminal `Complete` event, `Err` on a
/// terminal `Error` event, and `Ok(None)` on a non-terminal progress event.
fn process_turn_event(event: TurnEvent) -> Result<Option<TurnResult>, String> {
    match event.event {
        Some(turn_event::Event::Complete(TurnComplete {
            stop_reason,
            content,
            tool_calls,
            ..
        })) => {
            let reason = StopReason::try_from(stop_reason).unwrap_or(StopReason::Unspecified);
            Ok(Some(TurnResult {
                stop_reason: reason,
                content,
                tool_calls,
            }))
        }
        Some(turn_event::Event::Error(e)) => Err(format!("turn error {}: {}", e.code, e.message)),
        // ContentDelta, ToolUseStart, ToolUseInput are streaming progress
        // events — we skip them since the final TurnComplete has the
        // accumulated result.
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolset_proto::{ContentDelta, ToolUseInput, ToolUseStart, TurnError, TurnEvent};

    fn content_delta(text: &str) -> TurnEvent {
        TurnEvent {
            event: Some(turn_event::Event::ContentDelta(ContentDelta {
                text: text.into(),
            })),
        }
    }

    fn tool_start(id: &str, name: &str) -> TurnEvent {
        TurnEvent {
            event: Some(turn_event::Event::ToolUseStart(ToolUseStart {
                id: id.into(),
                name: name.into(),
            })),
        }
    }

    fn tool_input(partial: &str) -> TurnEvent {
        TurnEvent {
            event: Some(turn_event::Event::ToolUseInput(ToolUseInput {
                partial_json: partial.into(),
            })),
        }
    }

    fn text_delta_of(frame: &StreamItem) -> Option<&str> {
        match &frame.phase {
            Some(stream_item::Phase::Delta(ItemDelta {
                kind: Some(item_delta::Kind::TextDelta(t)),
            })) => Some(t),
            _ => None,
        }
    }

    fn tool_input_of(frame: &StreamItem) -> Option<&str> {
        match &frame.phase {
            Some(stream_item::Phase::Delta(ItemDelta {
                kind: Some(item_delta::Kind::ToolInputJson(j)),
            })) => Some(j),
            _ => None,
        }
    }

    fn is_text_start(frame: &StreamItem) -> bool {
        matches!(
            &frame.phase,
            Some(stream_item::Phase::Start(ItemStart {
                kind: Some(item_start::Kind::Text(_)),
            }))
        )
    }

    fn tool_name_of(frame: &StreamItem) -> Option<&str> {
        match &frame.phase {
            Some(stream_item::Phase::Start(ItemStart {
                kind: Some(item_start::Kind::ToolUse(t)),
            })) => Some(&t.name),
            _ => None,
        }
    }

    #[test]
    fn content_delta_opens_text_item_then_delta() {
        let mut state = EmitState::new("conv".into());
        let frames = stream_items_for(&content_delta("hello"), &mut state);
        assert_eq!(frames.len(), 2, "first delta emits Start then Delta");
        assert!(is_text_start(&frames[0]));
        assert_eq!(text_delta_of(&frames[1]), Some("hello"));
        // The start and delta share one item id.
        assert_eq!(frames[0].item_id, frames[1].item_id);
        // Envelope: conversation carried through.
        assert_eq!(frames[0].conversation_id, "conv");
    }

    #[test]
    fn subsequent_text_deltas_reuse_open_item_without_restart() {
        let mut state = EmitState::new("c".into());
        let first = stream_items_for(&content_delta("a"), &mut state);
        let second = stream_items_for(&content_delta("b"), &mut state);
        // Second delta emits only a Delta (no new Start) on the same item.
        assert_eq!(second.len(), 1);
        assert_eq!(text_delta_of(&second[0]), Some("b"));
        assert_eq!(second[0].item_id, first[0].item_id);
    }

    #[test]
    fn tool_start_carries_name_and_upstream_id() {
        let mut state = EmitState::new("c".into());
        let frames = stream_items_for(&tool_start("tc-1", "Bash"), &mut state);
        assert_eq!(frames.len(), 1);
        assert_eq!(tool_name_of(&frames[0]), Some("Bash"));
        assert_eq!(frames[0].item_id, "tc-1");
    }

    #[test]
    fn tool_input_delta_shares_tool_item_id() {
        let mut state = EmitState::new("c".into());
        let start = stream_items_for(&tool_start("tc-1", "Bash"), &mut state);
        let delta = stream_items_for(&tool_input(r#"{"cmd""#), &mut state);
        assert_eq!(delta.len(), 1);
        assert_eq!(tool_input_of(&delta[0]), Some(r#"{"cmd""#));
        assert_eq!(delta[0].item_id, start[0].item_id);
    }

    #[test]
    fn tool_start_closes_open_text_run() {
        let mut state = EmitState::new("c".into());
        stream_items_for(&content_delta("thinking"), &mut state);
        let frames = stream_items_for(&tool_start("tc-1", "Bash"), &mut state);
        // First frame stops the text item, second opens the tool item.
        assert_eq!(frames.len(), 2);
        assert!(matches!(frames[0].phase, Some(stream_item::Phase::Stop(_))));
        assert_eq!(tool_name_of(&frames[1]), Some("Bash"));
    }

    #[test]
    fn separate_text_runs_number_item_ids_sequentially() {
        // Two distinct text runs (text → tool → text) must open items
        // text-1 then text-2, so their ids do not collide.
        let mut state = EmitState::new("c".into());
        let first = stream_items_for(&content_delta("a"), &mut state);
        // Tool start ends the open text run so the next delta opens a NEW run.
        stream_items_for(&tool_start("t", "Bash"), &mut state);
        let second = stream_items_for(&content_delta("b"), &mut state);

        assert!(is_text_start(&first[0]));
        assert!(is_text_start(&second[0]));
        assert_eq!(first[0].item_id, "text-1");
        assert_eq!(second[0].item_id, "text-2");
        assert_ne!(first[0].item_id, second[0].item_id);
    }

    #[test]
    fn scrub_redacts_tool_name_and_input_before_emit() {
        // A ScrubSet holding a known secret redacts a tool ItemDelta's
        // argument JSON and a tool ItemStart's name as they cross the emit
        // seam.
        let scrub = shared::scrub::ScrubSet::from_secrets(&[("tok", "s3cr3t")]);
        assert!(!scrub.is_empty());

        let mut state = EmitState::new("c".into());
        stream_items_for(&tool_start("t", "Bash"), &mut state);
        let mut delta = stream_items_for(&tool_input(r#"{"token":"s3cr3t"}"#), &mut state)
            .pop()
            .unwrap();
        scrub_frame(&scrub, &mut delta);
        assert_eq!(tool_input_of(&delta), Some(r#"{"token":"[REDACTED:tok]"}"#));

        let mut state = EmitState::new("c".into());
        let mut start = stream_items_for(&tool_start("t", "s3cr3t"), &mut state)
            .pop()
            .unwrap();
        // Precondition: the ItemStart carries the secret name before scrubbing.
        assert_eq!(tool_name_of(&start), Some("s3cr3t"));
        scrub_frame(&scrub, &mut start);
        assert_eq!(tool_name_of(&start), Some("[REDACTED:tok]"));
    }

    #[tokio::test]
    async fn frames_emit_before_terminal_complete() {
        // A fake stream (delta → toolstart → toolinput → Complete) drives a
        // capturing sink; every progress frame must land BEFORE consume
        // returns the terminal result.
        use std::collections::VecDeque;
        use toolset_proto::TurnComplete;

        struct Script(VecDeque<TurnEvent>);
        #[async_trait::async_trait]
        impl crate::clients::TurnSource for Script {
            async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
                self.0.pop_front().map(Ok)
            }
        }

        struct Capturing(Vec<StreamItem>);
        #[async_trait::async_trait]
        impl StreamSink for Capturing {
            async fn emit(&mut self, item: StreamItem) {
                self.0.push(item);
            }
        }

        let mut src = Script(VecDeque::from(vec![
            content_delta("hi"),
            tool_start("t", "Bash"),
            tool_input("{}"),
            TurnEvent {
                event: Some(turn_event::Event::Complete(TurnComplete {
                    stop_reason: StopReason::EndTurn as i32,
                    content: vec![],
                    tool_calls: vec![],
                })),
            },
        ]));
        let mut sink = Capturing(vec![]);
        let mut emit = EmitState::new("c".into());
        let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_TURN_SCRUB__");
        let token = tokio_util::sync::CancellationToken::new();
        let result = consume_turn_stream_cancellable(
            &mut src,
            std::time::Duration::from_secs(45),
            &mut sink,
            &mut emit,
            &scrub,
            &token,
        )
        .await
        .unwrap();
        assert_eq!(result.stop_reason, StopReason::EndTurn);
        // text Start + text Delta + text Stop (tool_start closes the run) +
        // tool Start + tool-input Delta = 5 frames, all captured before the
        // terminal result was returned.
        assert_eq!(sink.0.len(), 5);
        assert!(is_text_start(&sink.0[0]));
        assert_eq!(text_delta_of(&sink.0[1]), Some("hi"));
        assert!(matches!(sink.0[2].phase, Some(stream_item::Phase::Stop(_))));
        assert_eq!(tool_name_of(&sink.0[3]), Some("Bash"));
        assert_eq!(tool_input_of(&sink.0[4]), Some("{}"));
    }

    #[test]
    fn process_error_event_returns_err_with_code_and_message() {
        let event = TurnEvent {
            event: Some(turn_event::Event::Error(TurnError {
                code: 42,
                message: "boom".to_string(),
            })),
        };
        let err = process_turn_event(event).unwrap_err();
        assert!(err.contains("42"));
        assert!(err.contains("boom"));
    }

    #[tokio::test]
    async fn idle_timeout_fires_on_inference_job_silence() {
        // An inference job that connected then went silent must fail the turn, not
        // hang it. A tiny real gap + a stalling source makes the timeout
        // fire fast.
        struct Stalling;
        #[async_trait::async_trait]
        impl crate::clients::TurnSource for Stalling {
            async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
                futures::future::pending().await
            }
        }
        let mut src = Stalling;
        let mut sink = NullSink;
        let mut emit = EmitState::new("c".into());
        let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_TURN_SCRUB__");
        let token = tokio_util::sync::CancellationToken::new();
        let res = consume_turn_stream_cancellable(
            &mut src,
            std::time::Duration::from_millis(50),
            &mut sink,
            &mut emit,
            &scrub,
            &token,
        )
        .await;
        assert!(matches!(res.unwrap_err(), TurnAbort::Ended(e) if e.contains("idle timeout")));
    }

    #[tokio::test]
    async fn stream_ends_without_terminal_returns_err() {
        // An inference-job stream that closes cleanly (EOF) without ever sending a
        // terminal Complete/Error must fail the turn, not hang or falsely
        // succeed — a dropped connection reads as end-of-stream, not an error.
        // Distinct from idle_timeout (which stalls): here next_event returns
        // None. A normal (not tiny) gap proves it's the clean-EOF arm, not the
        // timeout arm, that fires.
        struct Closed;
        #[async_trait::async_trait]
        impl crate::clients::TurnSource for Closed {
            async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
                None
            }
        }
        let mut src = Closed;
        let mut sink = NullSink;
        let mut emit = EmitState::new("c".into());
        let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_TURN_SCRUB__");
        let token = tokio_util::sync::CancellationToken::new();
        let res = consume_turn_stream_cancellable(
            &mut src,
            std::time::Duration::from_secs(45),
            &mut sink,
            &mut emit,
            &scrub,
            &token,
        )
        .await;
        assert!(matches!(
            res.unwrap_err(),
            TurnAbort::Ended(e) if e.contains("stream ended without TurnComplete")
        ));
    }

    // EARS: "When the harness receives a CancelTurn for an in-flight turn,
    // it shall stop the turn's LLM stream and abandon its in-flight work."
    // consume_turn_stream must race the turn against a cancellation signal and,
    // when fired, return a distinct Cancelled outcome WITHOUT draining the
    // remaining stream (abandon).
    #[tokio::test]
    async fn cancel_abandons_stream_before_it_completes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        // A source that never terminates on its own: it keeps yielding deltas.
        // Only a cancel can stop consume_turn_stream; if cancel is ignored the
        // test hangs / drains, which is the failure we want.
        struct Endless {
            polled: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl crate::clients::TurnSource for Endless {
            async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
                self.polled.fetch_add(1, Ordering::SeqCst);
                Some(Ok(content_delta("more")))
            }
        }

        let token = tokio_util::sync::CancellationToken::new();
        token.cancel(); // already cancelled before the first poll

        let polled = Arc::new(AtomicUsize::new(0));
        let mut src = Endless {
            polled: polled.clone(),
        };
        let mut sink = NullSink;
        let mut emit = EmitState::new("c".into());
        let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_TURN_SCRUB__");

        let outcome = consume_turn_stream_cancellable(
            &mut src,
            std::time::Duration::from_secs(45),
            &mut sink,
            &mut emit,
            &scrub,
            &token,
        )
        .await;

        // The cancel arm must win over an endless source and yield Cancelled.
        assert!(
            matches!(outcome, Err(TurnAbort::Cancelled)),
            "a fired cancel must abandon the turn as Cancelled, got {outcome:?}"
        );
    }
}
