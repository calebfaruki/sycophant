use tightbeam_proto::{
    content_block, ContentBlock, Message, StopReason, TextBlock, TurnRequest,
};

use crate::clients::TightbeamClient;
use crate::message_source::MessageSource;
use crate::tool_router::ToolRouter;
use crate::turn;

pub(crate) fn text_block(text: String) -> ContentBlock {
    ContentBlock {
        block: Some(content_block::Block::Text(TextBlock { text })),
    }
}

pub(crate) async fn run_single_agent(
    max_iterations: u32,
    tightbeam: &mut TightbeamClient,
    tool_router: &mut ToolRouter,
    message_source: &mut dyn MessageSource,
    agent_name: &str,
    system_prompt: &str,
) -> Result<(), String> {
    let tool_defs = tool_router.tool_definitions();
    let mut first_turn = true;

    loop {
        let content = message_source.next_message().await?;

        let user_msg = Message {
            role: "user".into(),
            content,
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
            agent: None,
        };

        let request = TurnRequest {
            system: Some(system_prompt.to_string()),
            tools: if first_turn {
                first_turn = false;
                tool_defs.clone()
            } else {
                vec![]
            },
            messages: vec![user_msg],
            agent: Some(agent_name.to_string()),
            model: None,
        };

        tool_loop(max_iterations, tightbeam, tool_router, request, agent_name).await?;
    }
}

pub(crate) async fn tool_loop(
    max_iterations: u32,
    tightbeam: &mut TightbeamClient,
    tool_router: &mut ToolRouter,
    initial_request: TurnRequest,
    agent: &str,
) -> Result<(), String> {
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
