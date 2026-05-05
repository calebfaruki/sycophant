use airlock_proto::ToolInfo;
use serde::Deserialize;
use tightbeam_proto::{content_block, ContentBlock, Message, StopReason, TurnRequest, TurnRole};

use crate::clients::TightbeamClient;
use crate::tool_router::ToolRouter;
use crate::turn;

pub(crate) const LLM_CALL_TOOL_NAME: &str = "llm_call";

#[derive(Deserialize)]
struct LlmCallArgs {
    system_prompt: String,
    query: String,
}

pub(crate) fn tool_definitions() -> Vec<ToolInfo> {
    vec![ToolInfo {
        name: LLM_CALL_TOOL_NAME.into(),
        description: "Dispatch a stateless sub-LLM call with a custom system prompt. \
                      Use this to delegate work to a different persona or specialist. \
                      The delegate has read access to the same files but no conversation history. \
                      Returns the delegate's final response as text."
            .into(),
        parameters_json: serde_json::json!({
            "type": "object",
            "properties": {
                "system_prompt": {
                    "type": "string",
                    "description": "System prompt for the delegate. The orchestrator typically reads an agent file from the Mainframe mount and passes its contents here."
                },
                "query": {
                    "type": "string",
                    "description": "The user-message-shaped query to send to the delegate. Construct whatever context the delegate needs into this field; the delegate will not see prior conversation history."
                }
            },
            "required": ["system_prompt", "query"]
        })
        .to_string(),
    }]
}

/// Dispatch an `llm_call` tool invocation. Spawns a delegate Tightbeam call with
/// `role = TurnRole::Delegate`, runs a tool loop for the delegate (with `llm_call`
/// excluded from the delegate's tool list — recursion is structurally blocked),
/// and returns the delegate's final assistant text.
pub(crate) async fn dispatch_llm_call(
    tightbeam: &mut TightbeamClient,
    tool_router: &mut ToolRouter,
    correlation_id: &str,
    input_json: &str,
    max_iterations: u32,
) -> Result<String, String> {
    let args: LlmCallArgs =
        serde_json::from_str(input_json).map_err(|e| format!("invalid llm_call arguments: {e}"))?;

    // Delegate inherits only the router-served tools (mainframe + airlock).
    // llm_call is a transponder built-in advertised at the orchestrator's call
    // site, never in the router — so the delegate naturally cannot invoke it.
    // Recursion blocking is structural at the router-vs-builtins boundary.
    let delegate_tools = tool_router.tool_definitions();

    let delegate_system = args.system_prompt.clone();
    let initial_request = TurnRequest {
        system: Some(args.system_prompt),
        tools: delegate_tools,
        messages: vec![Message {
            role: "user".into(),
            content: vec![text_block(args.query)],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        }],
        model: None,
        reply_channel: None,
        role: Some(TurnRole::Delegate as i32),
        correlation_id: Some(correlation_id.to_string()),
    };

    let mut stream = tightbeam.turn(initial_request).await?;
    let mut iterations = 0u32;

    loop {
        let result = turn::consume_turn_stream(&mut stream).await?;

        match result.stop_reason {
            StopReason::EndTurn | StopReason::MaxTokens => {
                return Ok(collect_text(&result.content));
            }
            StopReason::ToolUse => {
                iterations += 1;
                if iterations >= max_iterations {
                    return Err(format!(
                        "delegate iteration limit ({max_iterations}) reached"
                    ));
                }

                if result.tool_calls.is_empty() {
                    return Ok(collect_text(&result.content));
                }

                let mut tool_results = Vec::with_capacity(result.tool_calls.len());
                for tc in &result.tool_calls {
                    if tc.name == LLM_CALL_TOOL_NAME {
                        // Defense in depth: the delegate's tool list does not include
                        // llm_call, so the LLM should not be able to emit this. If we
                        // see it, refuse loudly rather than silently recursing.
                        return Err("delegate attempted recursive llm_call".into());
                    }
                    let response = tool_router.call_tool(&tc.name, &tc.input_json).await;
                    let (output, is_error) = match response {
                        Ok(resp) => (resp.output, resp.is_error),
                        Err(e) => (format!("tool call error: {e}"), true),
                    };
                    tool_results.push(Message {
                        role: "tool".into(),
                        content: vec![text_block(output)],
                        tool_calls: vec![],
                        tool_call_id: Some(tc.id.clone()),
                        is_error: if is_error { Some(true) } else { None },
                    });
                }

                let continuation = TurnRequest {
                    system: Some(delegate_system.clone()),
                    tools: vec![],
                    messages: tool_results,
                    model: None,
                    reply_channel: None,
                    role: Some(TurnRole::Delegate as i32),
                    correlation_id: Some(correlation_id.to_string()),
                };
                stream = tightbeam.turn(continuation).await?;
            }
            other => {
                return Err(format!("unexpected delegate stop reason: {other:?}"));
            }
        }
    }
}

fn text_block(text: String) -> ContentBlock {
    ContentBlock {
        block: Some(content_block::Block::Text(tightbeam_proto::TextBlock {
            text,
        })),
    }
}

fn collect_text(content: &[ContentBlock]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_includes_llm_call() {
        let defs = tool_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, LLM_CALL_TOOL_NAME);
        let schema: serde_json::Value = serde_json::from_str(&defs[0].parameters_json).unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["system_prompt"].is_object());
        assert!(schema["properties"]["query"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "system_prompt"));
        assert!(required.iter().any(|v| v == "query"));
    }

    #[test]
    fn parses_valid_args() {
        let args: LlmCallArgs =
            serde_json::from_str(r#"{"system_prompt":"You are alice.","query":"Hi"}"#).unwrap();
        assert_eq!(args.system_prompt, "You are alice.");
        assert_eq!(args.query, "Hi");
    }

    #[test]
    fn rejects_missing_field() {
        let result: Result<LlmCallArgs, _> = serde_json::from_str(r#"{"system_prompt":"alice"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn collect_text_joins_blocks_with_newlines_and_skips_leading_separator() {
        // Catches `delete !` on `if !buf.is_empty()` at collect_text:157.
        // Without the negation, the first block would prepend a newline.
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
        // A non-text content block contributes nothing.
        let blocks = vec![
            text_block("a".to_string()),
            ContentBlock { block: None },
            text_block("b".to_string()),
        ];
        assert_eq!(collect_text(&blocks), "a\nb");
    }
}
