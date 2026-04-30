use crate::types::{content_text, ContentBlock, Message, ToolDefinition};
use crate::{LlmProvider, ProviderConfig, StreamEvent};
use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use std::collections::HashSet;
use std::pin::Pin;

pub struct OpenAiProvider {
    client: reqwest::Client,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
        }
    }
}

fn content_block_to_api(block: &ContentBlock) -> Option<serde_json::Value> {
    match block {
        ContentBlock::Text { text } => Some(serde_json::json!({
            "type": "text",
            "text": text,
        })),
        ContentBlock::Image { media_type, data } => Some(serde_json::json!({
            "type": "image_url",
            "image_url": {
                "url": format!("data:{media_type};base64,{data}"),
            }
        })),
        ContentBlock::Thinking { .. } => None,
        ContentBlock::FileIncoming { .. } => {
            panic!("FileIncoming must be replaced before reaching provider")
        }
    }
}

fn build_api_messages(messages: &[Message], system: Option<&str>) -> Vec<serde_json::Value> {
    let mut api_messages = Vec::new();

    if let Some(sys) = system {
        api_messages.push(serde_json::json!({
            "role": "system",
            "content": sys,
        }));
    }

    for m in messages {
        if m.role == "tool" {
            let text = content_text(&m.content).unwrap_or("").to_string();
            api_messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id,
                "content": text,
            }));
        } else if let Some(ref tool_calls) = m.tool_calls {
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), "assistant".into());

            if let Some(ref blocks) = m.content {
                let text: String = blocks
                    .iter()
                    .filter_map(|b| b.as_text())
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    obj.insert("content".into(), text.into());
                }
            }

            let api_tool_calls: Vec<serde_json::Value> = tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": serde_json::to_string(&tc.input).unwrap_or_default(),
                        }
                    })
                })
                .collect();
            obj.insert(
                "tool_calls".into(),
                serde_json::Value::Array(api_tool_calls),
            );

            api_messages.push(serde_json::Value::Object(obj));
        } else {
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), m.role.clone().into());

            if let Some(ref blocks) = m.content {
                let api_blocks: Vec<serde_json::Value> =
                    blocks.iter().filter_map(content_block_to_api).collect();
                if api_blocks.len() == 1 && api_blocks[0].get("type") == Some(&"text".into()) {
                    obj.insert("content".into(), api_blocks[0]["text"].clone());
                } else {
                    obj.insert("content".into(), serde_json::Value::Array(api_blocks));
                }
            }

            api_messages.push(serde_json::Value::Object(obj));
        }
    }

    api_messages
}

fn build_api_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect()
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn call(
        &self,
        messages: &[Message],
        system: Option<&str>,
        tools: &[ToolDefinition],
        response_schema: Option<&str>,
        config: &ProviderConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, String>> + Send>>, String> {
        let mut body = serde_json::Map::new();
        body.insert("model".into(), config.model.clone().into());
        body.insert("max_tokens".into(), config.max_tokens.into());
        body.insert("stream".into(), true.into());

        body.insert(
            "messages".into(),
            serde_json::Value::Array(build_api_messages(messages, system)),
        );

        let api_tools = build_api_tools(tools);
        if !api_tools.is_empty() {
            body.insert("tools".into(), serde_json::Value::Array(api_tools));
        }

        if let Some(schema_str) = response_schema {
            let schema: serde_json::Value = serde_json::from_str(schema_str)
                .map_err(|e| format!("invalid response_schema_json: {e}"))?;
            body.insert(
                "response_format".into(),
                serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": SELECT_AGENT_SCHEMA_NAME,
                        "schema": schema,
                        "strict": true,
                    }
                }),
            );
        }

        let url = format!("{}/chat/completions", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
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
            Ok(Box::pin(stream::iter(content_to_structured_output(events))))
        } else {
            Ok(raw)
        }
    }
}

const SELECT_AGENT_SCHEMA_NAME: &str = "select_agent";

/// When a schema-driven call returns, the assistant content IS the structured
/// JSON. Pass `ContentDelta` events through (so the JSON lands in the
/// persisted conversation log for audit) and additionally emit a
/// `StructuredOutput` event so PKM can route on it deterministically.
fn content_to_structured_output(
    events: Vec<Result<StreamEvent, String>>,
) -> Vec<Result<StreamEvent, String>> {
    let mut out = Vec::with_capacity(events.len() + 1);
    let mut buf = String::new();

    for event in events {
        match event {
            Ok(StreamEvent::ContentDelta { ref text }) => {
                buf.push_str(text);
                out.push(event);
            }
            Ok(StreamEvent::Done { stop_reason }) => {
                if !buf.is_empty() {
                    out.push(Ok(StreamEvent::StructuredOutput { json: buf.clone() }));
                }
                out.push(Ok(StreamEvent::Done { stop_reason }));
                buf.clear();
            }
            other => out.push(other),
        }
    }

    if !buf.is_empty() {
        out.push(Ok(StreamEvent::StructuredOutput { json: buf }));
    }

    out
}

fn parse_sse_stream(
    response: reqwest::Response,
) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, String>> + Send>> {
    use std::collections::VecDeque;
    let byte_stream = response.bytes_stream();

    let event_stream = stream::unfold(
        (
            byte_stream,
            String::new(),
            HashSet::<u64>::new(),
            VecDeque::<StreamEvent>::new(),
        ),
        |(mut byte_stream, mut buffer, mut seen_tool_indices, mut pending)| async move {
            use futures::TryStreamExt;

            if let Some(event) = pending.pop_front() {
                return Some((Ok(event), (byte_stream, buffer, seen_tool_indices, pending)));
            }

            loop {
                if let Some(pos) = buffer.find("\n\n") {
                    let event_text = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    let events = parse_sse_event(&event_text, &mut seen_tool_indices);
                    if !events.is_empty() {
                        let mut iter = events.into_iter();
                        let first = iter.next().unwrap();
                        pending.extend(iter);
                        return Some((
                            Ok(first),
                            (byte_stream, buffer, seen_tool_indices, pending),
                        ));
                    }
                    continue;
                }

                match byte_stream.try_next().await {
                    Ok(Some(chunk)) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    Ok(None) => {
                        if !buffer.trim().is_empty() {
                            let events = parse_sse_event(&buffer, &mut seen_tool_indices);
                            buffer.clear();
                            let mut iter = events.into_iter();
                            if let Some(first) = iter.next() {
                                pending.extend(iter);
                                return Some((
                                    Ok(first),
                                    (byte_stream, buffer, seen_tool_indices, pending),
                                ));
                            }
                        }
                        return None;
                    }
                    Err(e) => {
                        return Some((
                            Err(format!("stream error: {e}")),
                            (byte_stream, buffer, seen_tool_indices, pending),
                        ));
                    }
                }
            }
        },
    );

    Box::pin(event_stream)
}

fn parse_sse_event(text: &str, seen_tool_indices: &mut HashSet<u64>) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    for line in text.lines() {
        let data = match line.strip_prefix("data: ") {
            Some(d) => d.trim(),
            None => continue,
        };

        // [DONE] is OpenAI's stream-terminator sentinel — no payload, just
        // skip. The byte stream's natural EOF drives the actual end of
        // iteration; we don't bail early here because some servers may
        // pipeline additional metadata after [DONE] in the same SSE event.
        if data == "[DONE]" {
            continue;
        }

        let parsed: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let choice = match parsed.get("choices").and_then(|c| c.get(0)) {
            Some(c) => c,
            None => continue,
        };

        // Always process delta first. Providers like Mistral emit content
        // (and tool_calls) in the same chunk as `finish_reason: stop`. An
        // early return on finish_reason — or processing finish_reason before
        // delta — would drop the trailing payload.
        if let Some(delta) = choice.get("delta") {
            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                if !content.is_empty() {
                    events.push(StreamEvent::ContentDelta {
                        text: content.to_string(),
                    });
                }
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tool_calls {
                    let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);

                    if !seen_tool_indices.contains(&index) {
                        seen_tool_indices.insert(index);
                        let id = tc
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        events.push(StreamEvent::ToolUseStart { id, name });
                    }

                    if let Some(args) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                    {
                        if !args.is_empty() {
                            events.push(StreamEvent::ToolUseInput {
                                json: args.to_string(),
                            });
                        }
                    }
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            let mapped = match reason {
                "stop" => "end_turn",
                "tool_calls" => "tool_use",
                "length" => "max_tokens",
                other => other,
            };
            events.push(StreamEvent::Done {
                stop_reason: mapped.to_string(),
            });
        }
    }

    events
}

#[cfg(test)]
mod openai_api {
    use super::*;
    use crate::types::ToolCall;

    #[test]
    fn user_message_converts() {
        let messages = vec![Message {
            role: "user".into(),
            content: Some(ContentBlock::text_content("Hello")),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
            agent: None,
        }];
        let api = build_api_messages(&messages, None);
        assert_eq!(api.len(), 1);
        assert_eq!(api[0]["role"], "user");
        assert_eq!(api[0]["content"], "Hello");
    }

    #[test]
    fn system_prompt_prepended() {
        let messages = vec![Message {
            role: "user".into(),
            content: Some(ContentBlock::text_content("Hi")),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
            agent: None,
        }];
        let api = build_api_messages(&messages, Some("You are helpful"));
        assert_eq!(api.len(), 2);
        assert_eq!(api[0]["role"], "system");
        assert_eq!(api[0]["content"], "You are helpful");
        assert_eq!(api[1]["role"], "user");
    }

    #[test]
    fn assistant_with_tool_calls_converts() {
        let messages = vec![Message {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call-1".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
            }]),
            tool_call_id: None,
            is_error: None,
            agent: None,
        }];
        let api = build_api_messages(&messages, None);
        assert_eq!(api[0]["role"], "assistant");
        let tc = &api[0]["tool_calls"][0];
        assert_eq!(tc["id"], "call-1");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "bash");
    }

    #[test]
    fn tool_result_converts() {
        let messages = vec![Message {
            role: "tool".into(),
            content: Some(ContentBlock::text_content("file list")),
            tool_calls: None,
            tool_call_id: Some("call-1".into()),
            is_error: Some(true),
            agent: None,
        }];
        let api = build_api_messages(&messages, None);
        assert_eq!(api[0]["role"], "tool");
        assert_eq!(api[0]["tool_call_id"], "call-1");
        assert_eq!(api[0]["content"], "file list");
        assert!(api[0].get("is_error").is_none());
    }

    #[test]
    fn thinking_blocks_skipped() {
        let messages = vec![Message {
            role: "assistant".into(),
            content: Some(vec![
                ContentBlock::thinking("deep thoughts"),
                ContentBlock::text("answer"),
            ]),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
            agent: None,
        }];
        let api = build_api_messages(&messages, None);
        assert_eq!(api[0]["content"], "answer");
    }

    #[test]
    fn tools_convert_to_function_format() {
        let tools = vec![ToolDefinition {
            name: "bash".into(),
            description: "Run a command".into(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let api = build_api_tools(&tools);
        assert_eq!(api[0]["type"], "function");
        assert_eq!(api[0]["function"]["name"], "bash");
        assert_eq!(api[0]["function"]["description"], "Run a command");
        assert_eq!(api[0]["function"]["parameters"]["type"], "object");
    }
}

#[cfg(test)]
mod sse_parsing {
    use super::*;

    #[test]
    fn content_delta_parses() {
        let text = r#"data: {"id":"x","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let mut seen = HashSet::new();
        let events = parse_sse_event(text, &mut seen);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ContentDelta { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected ContentDelta"),
        }
    }

    #[test]
    fn finish_reason_stop_maps_to_end_turn() {
        let text = r#"data: {"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let mut seen = HashSet::new();
        let events = parse_sse_event(text, &mut seen);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done { stop_reason } => assert_eq!(stop_reason, "end_turn"),
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn multi_data_lines_in_single_sse_event_all_processed() {
        // Some servers can pipeline multiple data: lines per SSE event. The
        // parser must process all of them. (Locks in no-early-return shape.)
        let text = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"foo\"},\"finish_reason\":null}]}\n\
                    data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"bar\"},\"finish_reason\":\"stop\"}]}\n\
                    data: [DONE]";
        let mut seen = HashSet::new();
        let events = parse_sse_event(text, &mut seen);
        let combined: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(combined, "foobar");
        assert!(
            events.iter().any(
                |e| matches!(e, StreamEvent::Done { stop_reason } if stop_reason == "end_turn")
            ),
            "Done must still be emitted even when followed by [DONE] sentinel"
        );
    }

    #[test]
    fn delta_and_finish_reason_in_same_chunk_both_emitted() {
        // Mistral schema-mode often emits the trailing JSON content AND
        // `finish_reason: stop` in a single chunk. The parser must emit BOTH
        // the ContentDelta and the Done event — losing the content truncates
        // structured outputs and breaks audit/replay.
        let text = r#"data: {"id":"x","choices":[{"index":0,"delta":{"content":"alice\"}"},"finish_reason":"stop"}]}"#;
        let mut seen = HashSet::new();
        let events = parse_sse_event(text, &mut seen);
        assert_eq!(events.len(), 2, "expected ContentDelta + Done");
        assert!(
            matches!(&events[0], StreamEvent::ContentDelta { text } if text == r#"alice"}"#),
            "first event must be ContentDelta with the content"
        );
        assert!(
            matches!(&events[1], StreamEvent::Done { stop_reason } if stop_reason == "end_turn"),
            "second event must be Done"
        );
    }

    #[test]
    fn finish_reason_tool_calls_maps_to_tool_use() {
        let text =
            r#"data: {"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
        let mut seen = HashSet::new();
        let events = parse_sse_event(text, &mut seen);
        match &events[0] {
            StreamEvent::Done { stop_reason } => assert_eq!(stop_reason, "tool_use"),
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn finish_reason_length_maps_to_max_tokens() {
        let text =
            r#"data: {"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"length"}]}"#;
        let mut seen = HashSet::new();
        let events = parse_sse_event(text, &mut seen);
        match &events[0] {
            StreamEvent::Done { stop_reason } => assert_eq!(stop_reason, "max_tokens"),
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn done_marker_returns_empty() {
        let text = "data: [DONE]";
        let mut seen = HashSet::new();
        let events = parse_sse_event(text, &mut seen);
        assert!(events.is_empty());
    }

    #[test]
    fn tool_call_start_emits_tool_use_start() {
        let text = r#"data: {"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"bash","arguments":""}}]},"finish_reason":null}]}"#;
        let mut seen = HashSet::new();
        let events = parse_sse_event(text, &mut seen);
        assert!(!events.is_empty());
        match &events[0] {
            StreamEvent::ToolUseStart { id, name } => {
                assert_eq!(id, "call-1");
                assert_eq!(name, "bash");
            }
            _ => panic!("expected ToolUseStart"),
        }
        assert!(seen.contains(&0));
    }

    #[test]
    fn tool_call_continuation_emits_tool_use_input() {
        let mut seen = HashSet::new();
        seen.insert(0);
        let text = r#"data: {"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\""}}]},"finish_reason":null}]}"#;
        let events = parse_sse_event(text, &mut seen);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolUseInput { json } => assert_eq!(json, "{\"command\""),
            _ => panic!("expected ToolUseInput"),
        }
    }

    #[test]
    fn multiple_tool_calls_tracked_by_index() {
        let mut seen = HashSet::new();

        let text1 = r#"data: {"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"bash","arguments":""}}]},"finish_reason":null}]}"#;
        let events1 = parse_sse_event(text1, &mut seen);
        assert!(matches!(&events1[0], StreamEvent::ToolUseStart { name, .. } if name == "bash"));

        let text2 = r#"data: {"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call-2","type":"function","function":{"name":"read","arguments":""}}]},"finish_reason":null}]}"#;
        let events2 = parse_sse_event(text2, &mut seen);
        assert!(matches!(&events2[0], StreamEvent::ToolUseStart { name, .. } if name == "read"));

        assert!(seen.contains(&0));
        assert!(seen.contains(&1));
    }

    #[test]
    fn content_to_structured_output_preserves_content_deltas_and_emits_structured() {
        let events = vec![
            Ok(StreamEvent::ContentDelta {
                text: r#"{"agent_n"#.into(),
            }),
            Ok(StreamEvent::ContentDelta {
                text: r#"ame":"alice"}"#.into(),
            }),
            Ok(StreamEvent::Done {
                stop_reason: "stop".into(),
            }),
        ];
        let out = content_to_structured_output(events);
        let structured = out
            .iter()
            .find_map(|e| match e {
                Ok(StreamEvent::StructuredOutput { json }) => Some(json.clone()),
                _ => None,
            })
            .expect("expected StructuredOutput");
        assert_eq!(structured, r#"{"agent_name":"alice"}"#);

        // Audit: ContentDelta events must pass through so the JSON lands in
        // the persisted conversation log.
        let combined: String = out
            .iter()
            .filter_map(|e| match e {
                Ok(StreamEvent::ContentDelta { text }) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            combined, r#"{"agent_name":"alice"}"#,
            "ContentDelta concatenation must equal the structured JSON"
        );

        let has_done = out
            .iter()
            .any(|e| matches!(e, Ok(StreamEvent::Done { .. })));
        assert!(has_done, "Done must still be emitted");
    }

    #[test]
    fn content_to_structured_output_emits_no_structured_when_empty() {
        let events = vec![Ok(StreamEvent::Done {
            stop_reason: "stop".into(),
        })];
        let out = content_to_structured_output(events);
        let has_structured = out
            .iter()
            .any(|e| matches!(e, Ok(StreamEvent::StructuredOutput { .. })));
        assert!(!has_structured);
    }
}
