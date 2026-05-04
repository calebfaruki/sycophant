use tightbeam_proto::{turn_event, ContentBlock, StopReason, ToolCall, TurnComplete, TurnEvent};
use tokio_stream::StreamExt;
use tonic::Streaming;

#[derive(Debug)]
pub(crate) struct TurnResult {
    pub stop_reason: StopReason,
    pub content: Vec<ContentBlock>,
    pub tool_calls: Vec<ToolCall>,
}

pub(crate) async fn consume_turn_stream(
    stream: &mut Streaming<TurnEvent>,
) -> Result<TurnResult, String> {
    while let Some(event) = stream.next().await {
        let event = event.map_err(|e| format!("stream error: {e}"))?;
        if let Some(result) = process_turn_event(event)? {
            return Ok(result);
        }
    }

    Err("stream ended without TurnComplete".into())
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
    use tightbeam_proto::{TurnError, TurnEvent};

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
}
