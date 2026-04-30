use tightbeam_proto::{content_block, ContentBlock, Message, StopReason, TextBlock, TurnRequest};

use crate::clients::TightbeamClient;
use crate::tool_router::ToolRouter;
use crate::turn;

pub(crate) fn text_block(text: String) -> ContentBlock {
    ContentBlock {
        block: Some(content_block::Block::Text(TextBlock { text })),
    }
}

pub(crate) async fn tool_loop(
    max_iterations: u32,
    tightbeam: &mut TightbeamClient,
    tool_router: &mut ToolRouter,
    initial_request: TurnRequest,
    agent: &str,
) -> Result<(), String> {
    let reply_channel = initial_request.reply_channel.clone();
    let mut stream = tightbeam.turn(initial_request).await?;
    let mut iterations = 0u32;

    loop {
        let result = turn::consume_turn_stream(&mut stream).await?;

        match result.stop_reason {
            StopReason::EndTurn => return Ok(()),
            StopReason::MaxTokens => {
                tracing::warn!("max_tokens reached, ending turn");
                return Ok(());
            }
            StopReason::ToolUse => {
                iterations += 1;
                if iterations >= max_iterations {
                    tracing::warn!(limit = max_iterations, "iteration limit reached, stopping");
                    return Ok(());
                }

                if result.tool_calls.is_empty() {
                    return Ok(());
                }

                let mut tool_result_messages = Vec::with_capacity(result.tool_calls.len());
                for tc in &result.tool_calls {
                    let response = tool_router.call_tool(&tc.name, &tc.input_json).await;
                    let (output, is_error) = match response {
                        Ok(resp) => (resp.output, resp.is_error),
                        Err(e) => (format!("tool call error: {e}"), true),
                    };

                    tool_result_messages.push(Message {
                        role: "tool".into(),
                        content: vec![text_block(output)],
                        tool_calls: vec![],
                        tool_call_id: Some(tc.id.clone()),
                        is_error: if is_error { Some(true) } else { None },
                        agent: None,
                    });
                }

                let continuation = TurnRequest {
                    system: None,
                    tools: vec![],
                    messages: tool_result_messages,
                    agent: Some(agent.to_string()),
                    model: None,
                    reply_channel: reply_channel.clone(),
                    role: None,
                    response_schema_json: None,
                };

                stream = tightbeam.turn(continuation).await?;
            }
            _ => {
                tracing::warn!(reason = ?result.stop_reason, "unexpected stop reason");
                return Ok(());
            }
        }
    }
}
