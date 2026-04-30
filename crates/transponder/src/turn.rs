use tightbeam_proto::{turn_event, ContentBlock, StopReason, ToolCall, TurnComplete, TurnEvent};
use tokio_stream::StreamExt;
use tonic::Streaming;

pub(crate) struct TurnResult {
    pub stop_reason: StopReason,
    pub content: Vec<ContentBlock>,
    pub tool_calls: Vec<ToolCall>,
    pub structured_json: Option<String>,
}

pub(crate) async fn consume_turn_stream(
    stream: &mut Streaming<TurnEvent>,
) -> Result<TurnResult, String> {
    while let Some(event) = stream.next().await {
        let event = event.map_err(|e| format!("stream error: {e}"))?;

        match event.event {
            Some(turn_event::Event::Complete(TurnComplete {
                stop_reason,
                content,
                tool_calls,
                structured_json,
            })) => {
                let reason = StopReason::try_from(stop_reason).unwrap_or(StopReason::Unspecified);
                return Ok(TurnResult {
                    stop_reason: reason,
                    content,
                    tool_calls,
                    structured_json,
                });
            }
            Some(turn_event::Event::Error(e)) => {
                return Err(format!("turn error {}: {}", e.code, e.message));
            }
            // ContentDelta, ToolUseStart, ToolUseInput are streaming progress
            // events — we skip them since the final TurnComplete has the
            // accumulated result.
            _ => continue,
        }
    }

    Err("stream ended without TurnComplete".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tightbeam_proto::{turn_event, StopReason};

    #[test]
    fn turn_result_carries_structured_json_field() {
        // Sanity: the field exists and Option<String> is the shape consumers expect.
        let result = TurnResult {
            stop_reason: StopReason::EndTurn,
            content: vec![],
            tool_calls: vec![],
            structured_json: Some(r#"{"agent_name":"alice"}"#.into()),
        };
        assert_eq!(
            result.structured_json.as_deref(),
            Some(r#"{"agent_name":"alice"}"#)
        );
    }

    #[test]
    fn complete_event_destructures_structured_json() {
        // Construct a TurnEvent and verify the destructure pattern reads structured_json.
        let event = turn_event::Event::Complete(TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![],
            tool_calls: vec![],
            structured_json: Some(r#"{"k":"v"}"#.into()),
        });
        match event {
            turn_event::Event::Complete(TurnComplete {
                structured_json, ..
            }) => assert_eq!(structured_json.as_deref(), Some(r#"{"k":"v"}"#)),
            _ => panic!("expected Complete variant"),
        }
    }
}
