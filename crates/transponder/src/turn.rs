use tightbeam_proto::{turn_event, ContentBlock, StopReason, ToolCall, TurnComplete, TurnEvent};
use tokio_stream::StreamExt;
use tonic::Streaming;

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

        match event.event {
            Some(turn_event::Event::Complete(TurnComplete {
                stop_reason,
                content,
                tool_calls,
            })) => {
                let reason =
                    StopReason::try_from(stop_reason).unwrap_or(StopReason::Unspecified);
                return Ok(TurnResult {
                    stop_reason: reason,
                    content,
                    tool_calls,
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
