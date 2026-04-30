use crate::types::{content_text, ContentBlock, Message, ToolDefinition};
use crate::{LlmProvider, ProviderConfig, StreamEvent};
use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use std::pin::Pin;

pub struct ClaudeProvider {
    client: reqwest::Client,
    base_url: String,
}

impl ClaudeProvider {
    pub fn new(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
        }
    }
}

fn content_block_to_api(block: &ContentBlock) -> serde_json::Value {
    match block {
        ContentBlock::Text { text } => serde_json::json!({
            "type": "text",
            "text": text,
        }),
        ContentBlock::Image { media_type, data } => serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data,
            }
        }),
        ContentBlock::Thinking { text } => serde_json::json!({
            "type": "thinking",
            "thinking": text,
        }),
        ContentBlock::FileIncoming { .. } => {
            panic!("FileIncoming must be replaced before reaching provider")
        }
    }
}

fn build_api_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|m| {
            let mut obj = serde_json::Map::new();

            if m.role == "tool" {
                obj.insert("role".into(), "user".into());
                if let Some(ref tool_call_id) = m.tool_call_id {
                    let text = content_text(&m.content).unwrap_or("").to_string();
                    obj.insert(
                        "content".into(),
                        serde_json::json!([{
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": text,
                        }]),
                    );
                }
            } else if let Some(ref tool_calls) = m.tool_calls {
                obj.insert("role".into(), m.role.clone().into());
                let mut content_blocks: Vec<serde_json::Value> = m
                    .content
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(content_block_to_api)
                    .collect();
                for tc in tool_calls {
                    content_blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.input,
                    }));
                }
                obj.insert("content".into(), serde_json::Value::Array(content_blocks));
            } else {
                obj.insert("role".into(), m.role.clone().into());
                if let Some(ref blocks) = m.content {
                    let api_blocks: Vec<serde_json::Value> =
                        blocks.iter().map(content_block_to_api).collect();
                    obj.insert("content".into(), serde_json::Value::Array(api_blocks));
                }
            }

            serde_json::Value::Object(obj)
        })
        .collect()
}

fn build_api_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            })
        })
        .collect()
}

#[async_trait]
impl LlmProvider for ClaudeProvider {
    async fn call(
        &self,
        messages: &[Message],
        system: Option<&str>,
        tools: &[ToolDefinition],
        response_schema: Option<&str>,
        config: &ProviderConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, String>> + Send>>, String> {
        let mut body = serde_json::Map::new();
        body.insert(
            "model".into(),
            serde_json::Value::String(config.model.clone()),
        );
        body.insert(
            "max_tokens".into(),
            serde_json::Value::Number(config.max_tokens.into()),
        );
        body.insert("stream".into(), serde_json::Value::Bool(true));

        if let Some(sys) = system {
            body.insert("system".into(), serde_json::Value::String(sys.to_string()));
        }

        body.insert(
            "messages".into(),
            serde_json::Value::Array(build_api_messages(messages)),
        );

        let mut api_tools = build_api_tools(tools);
        if let Some(schema_str) = response_schema {
            let schema: serde_json::Value = serde_json::from_str(schema_str)
                .map_err(|e| format!("invalid response_schema_json: {e}"))?;
            api_tools.push(serde_json::json!({
                "name": SELECT_AGENT_TOOL_NAME,
                "description": "Select the agent best fit to handle this user message.",
                "input_schema": schema,
            }));
            body.insert(
                "tool_choice".into(),
                serde_json::json!({"type": "tool", "name": SELECT_AGENT_TOOL_NAME}),
            );
        }
        if !api_tools.is_empty() {
            body.insert("tools".into(), serde_json::Value::Array(api_tools));
        }

        if let Some(ref budget) = config.thinking {
            body.insert(
                "thinking".into(),
                serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": budget.budget_tokens(),
                }),
            );
        }

        let url = format!("{}/messages", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("x-api-key", &config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("API error {status}: {body}"));
        }

        let raw = parse_sse_stream(response);
        if response_schema.is_some() {
            let events: Vec<_> = raw.collect().await;
            let processed = intercept_select_agent_events(events);
            Ok(Box::pin(stream::iter(processed)))
        } else {
            Ok(raw)
        }
    }
}

const SELECT_AGENT_TOOL_NAME: &str = "select_agent";

/// When a schema-driven call returns, the result arrives as a synthetic
/// `select_agent` tool_use. Strip those events, synthesize a `ContentDelta`
/// carrying the JSON (so it lands in the persisted conversation log for
/// audit), and emit a `StructuredOutput` so PKM can route on it.
fn intercept_select_agent_events(
    events: Vec<Result<StreamEvent, String>>,
) -> Vec<Result<StreamEvent, String>> {
    let mut out = Vec::with_capacity(events.len() + 2);
    let mut buf = String::new();
    let mut in_select = false;

    for event in events {
        match event {
            Ok(StreamEvent::ToolUseStart { ref name, .. }) if name == SELECT_AGENT_TOOL_NAME => {
                in_select = true;
            }
            Ok(StreamEvent::ToolUseInput { ref json }) if in_select => {
                buf.push_str(json);
            }
            Ok(StreamEvent::Done { stop_reason }) => {
                if !buf.is_empty() {
                    out.push(Ok(StreamEvent::ContentDelta { text: buf.clone() }));
                    out.push(Ok(StreamEvent::StructuredOutput { json: buf.clone() }));
                }
                out.push(Ok(StreamEvent::Done { stop_reason }));
                in_select = false;
                buf.clear();
            }
            other => out.push(other),
        }
    }

    if !buf.is_empty() {
        out.push(Ok(StreamEvent::ContentDelta { text: buf.clone() }));
        out.push(Ok(StreamEvent::StructuredOutput { json: buf }));
    }

    out
}

// --- Anthropic SSE parser (private) ---

fn parse_sse_stream(
    response: reqwest::Response,
) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, String>> + Send>> {
    let byte_stream = response.bytes_stream();

    let event_stream = stream::unfold(
        (byte_stream, String::new()),
        |(mut byte_stream, mut buffer)| async move {
            use futures::TryStreamExt;

            loop {
                if let Some(pos) = buffer.find("\n\n") {
                    let event_text = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    if let Some(event) = parse_sse_event(&event_text) {
                        return Some((Ok(event), (byte_stream, buffer)));
                    }
                    continue;
                }

                match byte_stream.try_next().await {
                    Ok(Some(chunk)) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    Ok(None) => {
                        if !buffer.trim().is_empty() {
                            if let Some(event) = parse_sse_event(&buffer) {
                                buffer.clear();
                                return Some((Ok(event), (byte_stream, buffer)));
                            }
                        }
                        return None;
                    }
                    Err(e) => {
                        return Some((Err(format!("stream error: {e}")), (byte_stream, buffer)));
                    }
                }
            }
        },
    );

    Box::pin(event_stream)
}

fn parse_sse_event(text: &str) -> Option<StreamEvent> {
    let mut event_type = None;
    let mut data_lines = Vec::new();

    for line in text.lines() {
        if let Some(stripped) = line.strip_prefix("event: ") {
            event_type = Some(stripped.trim().to_string());
        } else if let Some(stripped) = line.strip_prefix("data: ") {
            data_lines.push(stripped.to_string());
        }
    }

    let event_type = event_type?;
    let data = data_lines.join("\n");

    match event_type.as_str() {
        "content_block_delta" => {
            let parsed: serde_json::Value = serde_json::from_str(&data).ok()?;
            let delta = parsed.get("delta")?;
            let delta_type = delta.get("type")?.as_str()?;

            match delta_type {
                "text_delta" => {
                    let text = delta.get("text")?.as_str()?.to_string();
                    Some(StreamEvent::ContentDelta { text })
                }
                "thinking_delta" => {
                    let text = delta.get("thinking")?.as_str()?.to_string();
                    Some(StreamEvent::ThinkingDelta { text })
                }
                "input_json_delta" => {
                    let json = delta.get("partial_json")?.as_str()?.to_string();
                    Some(StreamEvent::ToolUseInput { json })
                }
                _ => None,
            }
        }
        "content_block_start" => {
            let parsed: serde_json::Value = serde_json::from_str(&data).ok()?;
            let block = parsed.get("content_block")?;
            let block_type = block.get("type")?.as_str()?;

            match block_type {
                "tool_use" => {
                    let id = block.get("id")?.as_str()?.to_string();
                    let name = block.get("name")?.as_str()?.to_string();
                    Some(StreamEvent::ToolUseStart { id, name })
                }
                "thinking" => Some(StreamEvent::ThinkingDelta {
                    text: String::new(),
                }),
                _ => None,
            }
        }
        "message_delta" => {
            let parsed: serde_json::Value = serde_json::from_str(&data).ok()?;
            let delta = parsed.get("delta")?;
            let stop_reason = delta.get("stop_reason")?.as_str()?.to_string();
            Some(StreamEvent::Done { stop_reason })
        }
        "message_stop" | "message_start" | "content_block_stop" | "ping" => None,
        _ => None,
    }
}

#[cfg(test)]
mod claude_api {
    use super::*;
    use crate::types::ToolCall;

    #[test]
    fn user_message_converts_to_api_format() {
        let messages = vec![Message {
            role: "user".into(),
            content: Some(ContentBlock::text_content("Hello")),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
            agent: None,
        }];
        let api = build_api_messages(&messages);
        assert_eq!(api.len(), 1);
        assert_eq!(api[0]["role"], "user");
        assert_eq!(api[0]["content"][0]["type"], "text");
        assert_eq!(api[0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn assistant_with_tool_calls_converts() {
        let messages = vec![Message {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc-1".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
            }]),
            tool_call_id: None,
            is_error: None,
            agent: None,
        }];
        let api = build_api_messages(&messages);
        let content = api[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["id"], "tc-1");
        assert_eq!(content[0]["name"], "bash");
    }

    #[test]
    fn tool_result_converts_to_user_with_tool_result_block() {
        let messages = vec![Message {
            role: "tool".into(),
            content: Some(ContentBlock::text_content("file list here")),
            tool_calls: None,
            tool_call_id: Some("tc-1".into()),
            is_error: None,
            agent: None,
        }];
        let api = build_api_messages(&messages);
        assert_eq!(api[0]["role"], "user");
        let content = api[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "tc-1");
        assert_eq!(content[0]["content"], "file list here");
    }

    #[test]
    fn image_block_converts_to_anthropic_format() {
        let messages = vec![Message {
            role: "user".into(),
            content: Some(vec![
                ContentBlock::text("Describe this"),
                ContentBlock::image("image/png", "iVBOR..."),
            ]),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
            agent: None,
        }];
        let api = build_api_messages(&messages);
        let content = api[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Describe this");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "iVBOR...");
    }

    #[test]
    #[should_panic(expected = "FileIncoming must be replaced")]
    fn file_incoming_panics_in_provider() {
        let messages = vec![Message {
            role: "user".into(),
            content: Some(vec![ContentBlock::file_incoming("f.png", "image/png", 1)]),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
            agent: None,
        }];
        build_api_messages(&messages);
    }

    #[test]
    fn tools_convert_to_api_format() {
        let tools = vec![ToolDefinition {
            name: "bash".into(),
            description: "Run a shell command".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }];
        let api = build_api_tools(&tools);
        assert_eq!(api.len(), 1);
        assert_eq!(api[0]["name"], "bash");
        assert_eq!(api[0]["description"], "Run a shell command");
    }
}

#[cfg(test)]
mod sse_parsing {
    use super::*;

    #[test]
    fn text_delta_parses() {
        let text = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}";
        let event = parse_sse_event(text).unwrap();
        match event {
            StreamEvent::ContentDelta { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected ContentDelta"),
        }
    }

    #[test]
    fn tool_use_start_parses() {
        let text = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tc-001\",\"name\":\"bash\",\"input\":{}}}";
        let event = parse_sse_event(text).unwrap();
        match event {
            StreamEvent::ToolUseStart { id, name } => {
                assert_eq!(id, "tc-001");
                assert_eq!(name, "bash");
            }
            _ => panic!("expected ToolUseStart"),
        }
    }

    #[test]
    fn input_json_delta_parses() {
        let text = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\"\"}}";
        let event = parse_sse_event(text).unwrap();
        match event {
            StreamEvent::ToolUseInput { json } => assert_eq!(json, "{\"command\""),
            _ => panic!("expected ToolUseInput"),
        }
    }

    #[test]
    fn message_delta_with_stop_reason_parses() {
        let text = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}";
        let event = parse_sse_event(text).unwrap();
        match event {
            StreamEvent::Done { stop_reason } => assert_eq!(stop_reason, "end_turn"),
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn message_stop_returns_none() {
        let text = "event: message_stop\ndata: {\"type\":\"message_stop\"}";
        assert!(parse_sse_event(text).is_none());
    }

    #[test]
    fn ping_returns_none() {
        let text = "event: ping\ndata: {}";
        assert!(parse_sse_event(text).is_none());
    }

    #[test]
    fn text_block_start_returns_none() {
        let text = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}";
        assert!(parse_sse_event(text).is_none());
    }

    #[test]
    fn tool_use_stop_reason_parses() {
        let text = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}";
        let event = parse_sse_event(text).unwrap();
        match event {
            StreamEvent::Done { stop_reason } => assert_eq!(stop_reason, "tool_use"),
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn intercept_select_agent_synthesizes_content_delta_for_audit() {
        let events = vec![
            Ok(StreamEvent::ToolUseStart {
                id: "tc-1".into(),
                name: "select_agent".into(),
            }),
            Ok(StreamEvent::ToolUseInput {
                json: r#"{"agent_n"#.into(),
            }),
            Ok(StreamEvent::ToolUseInput {
                json: r#"ame":"alice"}"#.into(),
            }),
            Ok(StreamEvent::Done {
                stop_reason: "tool_use".into(),
            }),
        ];
        let out = intercept_select_agent_events(events);

        let structured = out
            .iter()
            .find_map(|e| match e {
                Ok(StreamEvent::StructuredOutput { json }) => Some(json.clone()),
                _ => None,
            })
            .expect("expected StructuredOutput");
        assert_eq!(structured, r#"{"agent_name":"alice"}"#);

        // Audit: a synthetic ContentDelta must carry the JSON so it lands in
        // the persisted conversation log even though the synthetic tool_use
        // events are hidden.
        let content: String = out
            .iter()
            .filter_map(|e| match e {
                Ok(StreamEvent::ContentDelta { text }) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            content, r#"{"agent_name":"alice"}"#,
            "synthetic ContentDelta must carry the JSON for audit"
        );

        let has_tool_start = out.iter().any(|e| {
            matches!(
                e,
                Ok(StreamEvent::ToolUseStart { name, .. }) if name == "select_agent"
            )
        });
        assert!(
            !has_tool_start,
            "select_agent ToolUseStart must be swallowed"
        );
        let has_done = out
            .iter()
            .any(|e| matches!(e, Ok(StreamEvent::Done { .. })));
        assert!(has_done, "Done must still be emitted");
    }

    #[test]
    fn intercept_passes_through_when_no_select_agent() {
        let events = vec![
            Ok(StreamEvent::ContentDelta { text: "hi".into() }),
            Ok(StreamEvent::Done {
                stop_reason: "end_turn".into(),
            }),
        ];
        let out = intercept_select_agent_events(events);
        assert_eq!(out.len(), 2, "no schema → events pass through unchanged");
        let has_structured = out
            .iter()
            .any(|e| matches!(e, Ok(StreamEvent::StructuredOutput { .. })));
        assert!(!has_structured);
    }

    #[test]
    fn intercept_does_not_swallow_other_tool_calls() {
        let events = vec![
            Ok(StreamEvent::ToolUseStart {
                id: "tc-1".into(),
                name: "bash".into(),
            }),
            Ok(StreamEvent::ToolUseInput {
                json: r#"{"command":"ls"}"#.into(),
            }),
            Ok(StreamEvent::Done {
                stop_reason: "tool_use".into(),
            }),
        ];
        let out = intercept_select_agent_events(events);
        let has_bash = out.iter().any(|e| {
            matches!(
                e,
                Ok(StreamEvent::ToolUseStart { name, .. }) if name == "bash"
            )
        });
        assert!(has_bash, "non-schema tools must pass through");
    }
}
