use crate::merge::build_managed_body;
use crate::{LlmProvider, ProviderConfig, StreamEvent};
use async_trait::async_trait;
use base64::Engine;
use futures::stream::{self, Stream, StreamExt};
use proto_common::{content_block, content_text, Message, ToolDefinition};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::pin::Pin;
use std::time::Duration;

const MANAGED_OPENAI: &[&str] = &["model", "messages", "tools", "stream"];

fn build_openai_body(
    messages: &[Message],
    system: Option<&str>,
    tools: &[ToolDefinition],
    params: Option<&Map<String, Value>>,
    config: &ProviderConfig,
) -> (Map<String, Value>, Vec<String>) {
    let (mut body, clobbers) = build_managed_body(params, MANAGED_OPENAI);

    body.insert("model".into(), Value::String(config.model.clone()));
    body.insert("stream".into(), Value::Bool(true));
    body.insert(
        "messages".into(),
        Value::Array(build_api_messages(messages, system)),
    );
    let api_tools = build_api_tools(tools);
    if !api_tools.is_empty() {
        body.insert("tools".into(), Value::Array(api_tools));
    }

    (body, clobbers)
}

pub struct OpenAiProvider {
    client: reqwest::Client,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(base_url: String) -> Self {
        // See claude.rs for the rationale on http1_only() + timeouts.
        let client = reqwest::Client::builder()
            .http1_only()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client build");
        Self { client, base_url }
    }
}

fn content_block_to_api(block: &content_block::Block) -> Option<serde_json::Value> {
    match block {
        content_block::Block::Text(t) => Some(serde_json::json!({
            "type": "text",
            "text": t.text,
        })),
        content_block::Block::Image(img) => {
            let media_type = &img.media_type;
            let data = base64::engine::general_purpose::STANDARD.encode(&img.data);
            Some(serde_json::json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{media_type};base64,{data}"),
                }
            }))
        }
        content_block::Block::Thinking(_) => None,
        content_block::Block::File(_) => {
            unreachable!(
                "FileIncoming must be replaced by the controller before reaching the provider"
            )
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
            let text = content_text(&m.content);
            api_messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id,
                "content": text,
            }));
        } else if !m.tool_calls.is_empty() {
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), "assistant".into());

            let text = content_text(&m.content);
            if !text.is_empty() {
                obj.insert("content".into(), text.into());
            }

            let api_tool_calls: Vec<serde_json::Value> = m
                .tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.input_json,
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

            if !m.content.is_empty() {
                let api_blocks: Vec<serde_json::Value> = m
                    .content
                    .iter()
                    .filter_map(|b| b.block.as_ref().and_then(content_block_to_api))
                    .collect();
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
            let parameters: Value = serde_json::from_str(&t.parameters_json).unwrap_or(Value::Null);
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": parameters,
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
        params: Option<&Map<String, Value>>,
        config: &ProviderConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, String>> + Send>>, String> {
        let (body, clobbers) = build_openai_body(messages, system, tools, params, config);

        let url = format!("{}/chat/completions", self.base_url);
        let mut request = self.client.post(&url);
        // An empty key is a destination that authenticates nobody, not a token.
        // `Bearer ` with nothing after it is malformed, and a gateway may refuse it.
        if !config.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", config.api_key));
        }
        let response = request
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

        let warnings_stream = crate::merge::warning_stream_for(clobbers);
        let sse = parse_sse_stream(response);
        Ok(Box::pin(warnings_stream.chain(sse)))
    }

    fn managed_fields(&self) -> &'static [&'static str] {
        MANAGED_OPENAI
    }
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
                if let Some((event_text, rest)) = crate::split_first_sse_event(&buffer) {
                    buffer = rest;
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

            // Reasoning-model thinking. OpenRouter (deepseek-v4-flash) spells it
            // `reasoning`; DeepSeek-native / vLLM / SGLang use `reasoning_content`.
            // Without this the whole thinking phase is dropped: no live thinking
            // panel and no Thinking block in the assembled Complete.
            if let Some(reasoning) = delta
                .get("reasoning")
                .and_then(|r| r.as_str())
                .or_else(|| delta.get("reasoning_content").and_then(|r| r.as_str()))
            {
                if !reasoning.is_empty() {
                    events.push(StreamEvent::ThinkingDelta {
                        text: reasoning.to_string(),
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
mod reasoning_model_stream {
    use super::*;

    /// CONTENT-LOSS VALIDATION: a reasoning model (e.g. deepseek-v4-flash via
    /// OpenRouter) puts its thinking in `delta.reasoning` (or `reasoning_content`)
    /// with `content` null/empty. `parse_sse_event` handles `content`,
    /// `tool_calls`, and `finish_reason` but historically NOT `reasoning`, so the
    /// whole thinking phase yielded zero events and the model's reasoning never
    /// reached the assembled Complete's Thinking block or the audit log. (This is
    /// content loss, not a liveness bug — the LLM Job's 10s heartbeat keeps the
    /// harness's idle gap reset during reasoning silence.)
    ///
    /// This test asserts the FIX: a `reasoning` delta surfaces as a
    /// `ThinkingDelta`, which `collect_thinking` folds into the final Complete.
    #[test]
    fn reasoning_delta_surfaces_as_progress_event() {
        // Verbatim shape from OpenRouter/GMICloud for deepseek-v4-flash.
        let sse = r#"data: {"id":"gen-1","object":"chat.completion.chunk","model":"deepseek/deepseek-v4-flash","choices":[{"index":0,"delta":{"content":null,"role":"assistant","reasoning":"The user wants me to"},"finish_reason":null}]}"#;
        let mut seen = HashSet::new();
        let events = parse_sse_event(sse, &mut seen);
        assert!(
            !events.is_empty(),
            "a reasoning delta must surface as a thinking event, not be silently \
             dropped (silent drop loses the model's reasoning from the audit log)"
        );
        assert!(
            matches!(&events[0], StreamEvent::ThinkingDelta { text } if text == "The user wants me to"),
            "reasoning text must surface as a ThinkingDelta, got {:?}",
            events.first()
        );
    }

    /// DeepSeek-native / vLLM / SGLang spell the field `reasoning_content`
    /// instead of `reasoning`. The alias must cover it or self-hosted reasoning
    /// models silently lose their whole thinking phase.
    #[test]
    fn reasoning_content_alias_surfaces_as_thinking() {
        let sse = r#"data: {"choices":[{"index":0,"delta":{"content":null,"reasoning_content":"Let me think"},"finish_reason":null}]}"#;
        let mut seen = HashSet::new();
        let events = parse_sse_event(sse, &mut seen);
        assert!(
            matches!(events.first(), Some(StreamEvent::ThinkingDelta { text }) if text == "Let me think"),
            "reasoning_content must surface as a ThinkingDelta, got {:?}",
            events.first()
        );
    }

    /// Mixed OpenAI-compat proxies emit BOTH keys in one delta with `reasoning`
    /// JSON-null and the text in `reasoning_content`. A present-but-null
    /// `reasoning` must NOT shadow the live `reasoning_content` (the `or_else`
    /// fires on absent-key only, so field selection must be
    /// `.and_then(as_str).or_else(...)`, not `.or_else(...).and_then(as_str)`).
    #[test]
    fn null_reasoning_does_not_shadow_reasoning_content() {
        let sse = r#"data: {"choices":[{"index":0,"delta":{"content":null,"reasoning":null,"reasoning_content":"still thinking"},"finish_reason":null}]}"#;
        let mut seen = HashSet::new();
        let events = parse_sse_event(sse, &mut seen);
        assert!(
            matches!(events.first(), Some(StreamEvent::ThinkingDelta { text }) if text == "still thinking"),
            "a null `reasoning` must not shadow a live `reasoning_content`, got {:?}",
            events.first()
        );
    }
}

#[cfg(test)]
mod openai_body {
    use super::*;

    fn cfg() -> ProviderConfig {
        ProviderConfig {
            model: "gpt-4o".into(),
            api_key: "sk-test".into(),
        }
    }

    #[test]
    fn body_no_max_tokens_default() {
        let (body, _) = build_openai_body(&[], None, &[], None, &cfg());
        assert!(
            !body.contains_key("max_tokens"),
            "OpenAI body should not default max_tokens (pure pass-through)"
        );
    }

    #[test]
    fn body_max_tokens_passes_through_when_set() {
        let mut params = serde_json::Map::new();
        params.insert("max_tokens".into(), serde_json::json!(2048));
        let (body, _) = build_openai_body(&[], None, &[], Some(&params), &cfg());
        assert_eq!(
            body.get("max_tokens"),
            Some(&serde_json::Value::Number(2048.into()))
        );
    }

    #[test]
    fn body_clobbers_principal_tools_and_reports() {
        let mut params = serde_json::Map::new();
        params.insert("tools".into(), serde_json::json!(["forged"]));
        let tools = vec![ToolDefinition {
            name: "real".into(),
            description: "".into(),
            parameters_json: "{}".into(),
        }];
        let (body, clobbers) = build_openai_body(&[], None, &tools, Some(&params), &cfg());
        assert_eq!(clobbers, vec!["tools".to_string()]);
        // Sycophant's tools (the real ones) overwrite the principal's forged list.
        assert!(
            body.get("tools")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().any(|t| t.get("function").is_some()))
                .unwrap_or(false),
            "tools field must reflect sycophant's real tools, not the forged params"
        );
    }

    #[test]
    fn body_omits_tools_when_empty_params_and_empty_tools() {
        let (body, _) = build_openai_body(&[], None, &[], None, &cfg());
        assert!(
            !body.contains_key("tools"),
            "tools field should be absent when no tools are provided and not in params"
        );
    }

    #[test]
    fn body_passes_through_unmanaged_temperature() {
        let mut params = serde_json::Map::new();
        params.insert("temperature".into(), serde_json::json!(0.7));
        let (body, clobbers) = build_openai_body(&[], None, &[], Some(&params), &cfg());
        assert!(clobbers.is_empty());
        assert_eq!(
            body.get("temperature"),
            Some(&serde_json::Value::Number(
                serde_json::Number::from_f64(0.7).unwrap()
            ))
        );
    }
}

#[cfg(test)]
mod openai_api {
    use super::*;
    use proto_common::{
        content_block, image_block, text_block, text_content, ThinkingBlock, ToolCall,
    };

    fn thinking_block(text: &str) -> proto_common::ContentBlock {
        proto_common::ContentBlock {
            block: Some(content_block::Block::Thinking(ThinkingBlock {
                text: text.into(),
            })),
        }
    }

    #[test]
    fn user_message_converts() {
        let messages = vec![Message {
            role: "user".into(),
            content: text_content("Hello"),
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
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
            content: text_content("Hi"),
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
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
            content: vec![],
            tool_calls: vec![ToolCall {
                id: "call-1".into(),
                name: "bash".into(),
                input_json: r#"{"command":"ls"}"#.into(),
            }],
            tool_call_id: None,
            is_error: None,
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
            content: text_content("file list"),
            tool_calls: vec![],
            tool_call_id: Some("call-1".into()),
            is_error: Some(true),
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
            content: vec![thinking_block("deep thoughts"), text_block("answer".into())],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        }];
        let api = build_api_messages(&messages, None);
        assert_eq!(api[0]["content"], "answer");
    }

    #[test]
    fn tools_convert_to_function_format() {
        let tools = vec![ToolDefinition {
            name: "bash".into(),
            description: "Run a command".into(),
            parameters_json: r#"{"type":"object"}"#.into(),
        }];
        let api = build_api_tools(&tools);
        assert_eq!(api[0]["type"], "function");
        assert_eq!(api[0]["function"]["name"], "bash");
        assert_eq!(api[0]["function"]["description"], "Run a command");
        assert_eq!(api[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn assistant_with_tool_calls_and_text_content_includes_content_field() {
        // Catches `delete !` on `if !text.is_empty()` — without the negation,
        // empty text would erroneously insert content="" while non-empty text
        // would skip insertion.
        let messages = vec![Message {
            role: "assistant".into(),
            content: text_content("preamble before tool"),
            tool_calls: vec![ToolCall {
                id: "call-1".into(),
                name: "bash".into(),
                input_json: "{}".into(),
            }],
            tool_call_id: None,
            is_error: None,
        }];
        let api = build_api_messages(&messages, None);
        assert_eq!(api[0]["content"], "preamble before tool");
    }

    #[test]
    fn assistant_with_tool_calls_and_empty_text_omits_content_field() {
        // The companion: when text IS empty, the content field must NOT be
        // present. Together with the previous test, this catches the `!`.
        let messages = vec![Message {
            role: "assistant".into(),
            content: vec![],
            tool_calls: vec![ToolCall {
                id: "call-1".into(),
                name: "bash".into(),
                input_json: "{}".into(),
            }],
            tool_call_id: None,
            is_error: None,
        }];
        let api = build_api_messages(&messages, None);
        assert!(api[0].as_object().unwrap().get("content").is_none());
    }

    #[test]
    fn user_message_with_single_image_block_uses_array_content() {
        // Catches `&&` -> `||` on the single-text-block collapse condition.
        // With `&&`: 1 block AND type=text → string; otherwise → array.
        // With `||`: 1 block OR type=text → string-ish — would attempt
        // api_blocks[0]["text"] on an image and return Null, breaking content.
        let messages = vec![Message {
            role: "user".into(),
            content: vec![image_block("image/png".into(), b"hello".to_vec())],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        }];
        let api = build_api_messages(&messages, None);
        assert!(
            api[0]["content"].is_array(),
            "single non-text block must be wrapped in an array"
        );
        let arr = api[0]["content"].as_array().unwrap();
        assert_eq!(arr[0]["type"], "image_url");
    }

    #[test]
    fn user_message_with_two_text_blocks_uses_array_content() {
        // Companion: multiple text blocks must remain an array (catches the
        // alternative `&&` mutation flip too).
        let messages = vec![Message {
            role: "user".into(),
            content: vec![text_block("first".into()), text_block("second".into())],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        }];
        let api = build_api_messages(&messages, None);
        assert!(api[0]["content"].is_array());
        let arr = api[0]["content"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
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
}
