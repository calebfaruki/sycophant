use std::time::Duration;

use hangar_proto::{turn_event, ContentBlock, StopReason, ToolCall, TurnComplete, TurnEvent};

use crate::clients::TurnSource;

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
                if let Some(result) = process_turn_event(event)? {
                    return Ok(result);
                }
            }
        }
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
    use hangar_proto::{TurnError, TurnEvent};

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
        let res = consume_turn_stream(&mut src, std::time::Duration::from_millis(50)).await;
        assert!(res.unwrap_err().contains("idle timeout"));
    }
}
