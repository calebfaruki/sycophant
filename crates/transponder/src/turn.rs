use std::time::Duration;

use hangar_proto::{turn_event, ContentBlock, StopReason, ToolCall, TurnComplete, TurnEvent};
use proto_common::{
    item_delta, item_start, stream_item, ItemDelta, ItemStart, ItemStop, StreamItem, TextItem,
    ToolUseItem,
};

use crate::clients::TurnSource;

/// Sink for streamed activity frames produced during a turn. The orchestrator
/// path backs it with a gateway RPC; the sub-agent path backs it with a no-op.
#[async_trait::async_trait]
pub(crate) trait StreamSink: Send {
    async fn emit(&mut self, item: StreamItem);
}

/// A sink that drops every frame. Used where streamed items are out of scope
/// (sub-agent dispatch) or absent (no reply channel).
pub(crate) struct NullSink;

#[async_trait::async_trait]
impl StreamSink for NullSink {
    async fn emit(&mut self, _item: StreamItem) {}
}

/// Per-turn emit bookkeeping: the monotonic `workspace_seq` and which item
/// runs are currently open. Threaded across every `consume_turn_stream` call
/// within one `llm_loop` so the sequence stays monotonic and item ids stay
/// stable across continuation turns.
pub(crate) struct EmitState {
    pub conversation_id: String,
    /// Monotonic, 1-indexed per workspace. Pre-increment on each emitted frame.
    seq: u64,
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
            seq: 0,
            text_item: None,
            tool_item: None,
            text_runs: 0,
        }
    }

    fn next_frame(&mut self, item_id: String, phase: stream_item::Phase) -> StreamItem {
        self.seq += 1;
        StreamItem {
            workspace_seq: self.seq,
            event_id: uuid::Uuid::new_v4().to_string(),
            item_id,
            conversation_id: self.conversation_id.clone(),
            phase: Some(phase),
        }
    }
}

/// Map one hangar turn-event to the streamed-item frames it produces. Pure
/// aside from the `state` bookkeeping it advances (seq, open-run ids).
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

/// Default idle-gap: the maximum silence between worker events
/// (deltas / heartbeats / Complete) before a turn is treated as wedged.
/// Sized well above the worker's 10s heartbeat so a slow-but-alive turn is
/// never reaped, while a genuinely silent (connected-but-hung) worker trips
/// it. Used by the sub-agent path; the orchestrator loop takes its gap from
/// config via `LoopMode`.
pub(crate) const DEFAULT_IDLE_GAP: Duration = Duration::from_secs(45);

#[derive(Debug)]
pub(crate) struct TurnResult {
    pub stop_reason: StopReason,
    pub content: Vec<ContentBlock>,
    pub tool_calls: Vec<ToolCall>,
}

/// Consume a turn's event stream until a terminal `Complete`. `idle_gap`
/// bounds the silence between events (reset every iteration), so a worker
/// that connected then wedged is failed instead of awaited forever — while
/// the worker's heartbeat keeps a legitimately-long turn alive. An
/// idle-timeout returns `Err` like any other stream end; the caller's
/// no-restart policy turns that into "fail this turn, keep serving".
pub(crate) async fn consume_turn_stream(
    source: &mut dyn TurnSource,
    idle_gap: Duration,
    sink: &mut dyn StreamSink,
    emit: &mut EmitState,
    scrub: &shared::scrub::ScrubSet,
) -> Result<TurnResult, String> {
    loop {
        match tokio::time::timeout(idle_gap, source.next_event()).await {
            Err(_) => {
                return Err(format!(
                    "idle timeout: no worker progress in {}s",
                    idle_gap.as_secs()
                ))
            }
            Ok(None) => return Err("stream ended without TurnComplete".into()),
            Ok(Some(event)) => {
                let event = event?;
                // Streamed activity frames emit DURING the turn, before the
                // terminal Complete/Error is returned to the caller.
                for mut frame in stream_items_for(&event, emit) {
                    scrub_frame(scrub, &mut frame);
                    sink.emit(frame).await;
                }
                if let Some(result) = process_turn_event(event)? {
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
// scrub seam wired; no-op until transponder holds a secret registry
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
    use hangar_proto::{ContentDelta, ToolUseInput, ToolUseStart, TurnError, TurnEvent};

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
        // Envelope: conversation carried through, seq strictly increasing.
        assert_eq!(frames[0].conversation_id, "conv");
        assert!(frames[1].workspace_seq > frames[0].workspace_seq);
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
    fn empty_content_delta_emits_no_frame() {
        // Provider heartbeats carry empty text and must not open a text item.
        let mut state = EmitState::new("c".into());
        let frames = stream_items_for(&content_delta(""), &mut state);
        assert!(frames.is_empty());
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
        assert!(matches!(
            frames[0].phase,
            Some(stream_item::Phase::Stop(_))
        ));
        assert_eq!(tool_name_of(&frames[1]), Some("Bash"));
    }

    #[test]
    fn workspace_seq_strictly_increases_across_events() {
        let mut state = EmitState::new("c".into());
        let mut seqs = Vec::new();
        for ev in [
            content_delta("a"),
            content_delta("b"),
            tool_start("t", "T"),
            tool_input("{}"),
        ] {
            for f in stream_items_for(&ev, &mut state) {
                seqs.push(f.workspace_seq);
            }
        }
        assert!(seqs.windows(2).all(|w| w[1] > w[0]), "seqs: {seqs:?}");
        assert_eq!(seqs[0], 1, "workspace_seq is 1-indexed");
    }

    #[test]
    fn tool_input_without_open_tool_emits_nothing() {
        let mut state = EmitState::new("c".into());
        let frames = stream_items_for(&tool_input("{}"), &mut state);
        assert!(frames.is_empty());
    }

    #[test]
    fn scrub_redacts_tool_input_delta_before_emit() {
        // A ScrubSet holding a known secret redacts a tool ItemDelta's
        // argument JSON. Text frames are untouched.
        std::env::set_var("TEST_TURN_SCRUB_SECRET", "s3cr3t");
        std::env::set_var(
            "TEST_TURN_SCRUB",
            r#"[{"name":"tok","env":"TEST_TURN_SCRUB_SECRET"}]"#,
        );
        let scrub = shared::scrub::ScrubSet::from_env_var("TEST_TURN_SCRUB");
        std::env::remove_var("TEST_TURN_SCRUB");
        std::env::remove_var("TEST_TURN_SCRUB_SECRET");
        assert!(!scrub.is_empty());

        let mut state = EmitState::new("c".into());
        stream_items_for(&tool_start("t", "Bash"), &mut state);
        let mut delta = stream_items_for(&tool_input(r#"{"token":"s3cr3t"}"#), &mut state)
            .pop()
            .unwrap();
        scrub_frame(&scrub, &mut delta);
        assert_eq!(tool_input_of(&delta), Some(r#"{"token":"[REDACTED:tok]"}"#));
    }

    #[tokio::test]
    async fn frames_emit_before_terminal_complete() {
        // A fake stream (delta → toolstart → toolinput → Complete) drives a
        // capturing sink; every progress frame must land BEFORE consume
        // returns the terminal result. Mutant: move emit after the terminal
        // return → the sink is empty and this fails.
        use std::collections::VecDeque;
        use hangar_proto::TurnComplete;

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
        let result = consume_turn_stream(
            &mut src,
            std::time::Duration::from_secs(45),
            &mut sink,
            &mut emit,
            &scrub,
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

    #[test]
    fn process_complete_event_returns_result() {
        let event = TurnEvent {
            event: Some(turn_event::Event::Complete(TurnComplete {
                stop_reason: 0,
                content: vec![],
                tool_calls: vec![],
            })),
        };
        let result = process_turn_event(event).unwrap().expect("should be Some");
        assert!(result.content.is_empty());
        assert!(result.tool_calls.is_empty());
    }

    #[test]
    fn process_non_terminal_event_returns_none() {
        let event = TurnEvent { event: None };
        let result = process_turn_event(event).unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn idle_timeout_fires_on_worker_silence() {
        // A worker that connected then went silent must fail the turn, not
        // hang it. A tiny real gap + a stalling source makes the timeout
        // fire fast. Mutant: remove the timeout wrapper → next_event()
        // pends forever and this test hangs.
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
        let res = consume_turn_stream(
            &mut src,
            std::time::Duration::from_millis(50),
            &mut sink,
            &mut emit,
            &scrub,
        )
        .await;
        assert!(res.unwrap_err().contains("idle timeout"));
    }

    #[tokio::test]
    async fn stream_ends_without_terminal_returns_err() {
        // A worker stream that closes cleanly (EOF) without ever sending a
        // terminal Complete/Error must fail the turn, not hang or falsely
        // succeed — a dropped connection reads as end-of-stream, not an error.
        // Distinct from idle_timeout (which stalls): here next_event returns
        // None. A normal (not tiny) gap proves it's the clean-EOF arm (line 40),
        // not the timeout arm, that fires. Mutant: fold the Ok(None) arm into
        // the timeout arm → wrong message, red.
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
        let res = consume_turn_stream(
            &mut src,
            std::time::Duration::from_secs(45),
            &mut sink,
            &mut emit,
            &scrub,
        )
        .await;
        assert!(res
            .unwrap_err()
            .contains("stream ended without TurnComplete"));
    }
}
