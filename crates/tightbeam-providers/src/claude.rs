use crate::merge::build_managed_body;
use crate::types::{content_text, ContentBlock, Message, ToolDefinition};
use crate::{LlmProvider, ProviderConfig, StreamEvent};
use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use serde_json::{Map, Value};
use std::pin::Pin;
use std::time::Duration;

const MANAGED_ANTHROPIC: &[&str] = &["model", "messages", "system", "tools", "stream"];

/// Pure body-build helper, separated for testability. Returns the request
/// body and a list of clobbered managed-field names (one warning per).
fn build_anthropic_body(
    messages: &[Message],
    system: Option<&str>,
    tools: &[ToolDefinition],
    params: Option<&Map<String, Value>>,
    config: &ProviderConfig,
) -> (Map<String, Value>, Vec<String>) {
    let (mut body, clobbers) = build_managed_body(params, MANAGED_ANTHROPIC);

    // Anthropic requires max_tokens in the body. Default if params lacks it
    // (operators/principals can override via params; not in managed list).
    body.entry("max_tokens".to_string())
        .or_insert_with(|| 8192.into());

    // Write managed values last so they overwrite any clobbered principal entries.
    body.insert("model".into(), Value::String(config.model.clone()));
    body.insert("stream".into(), Value::Bool(true));
    body.insert(
        "messages".into(),
        Value::Array(build_api_messages(messages)),
    );
    if let Some(sys) = system {
        // Send `system` as an array of text blocks so we can attach
        // `cache_control` — Anthropic prompt caching marks the
        // longest cacheable prefix at each breakpoint and reads it
        // back at 0.1× input cost on subsequent requests within the
        // 5-minute TTL. The string form of `system` is also accepted
        // by the API but does not allow cache_control.
        body.insert(
            "system".into(),
            serde_json::json!([
                {
                    "type": "text",
                    "text": sys,
                    "cache_control": { "type": "ephemeral" }
                }
            ]),
        );
    }
    let api_tools = build_api_tools(tools);
    if !api_tools.is_empty() {
        body.insert("tools".into(), Value::Array(api_tools));
    }

    (body, clobbers)
}

pub struct ClaudeProvider {
    client: reqwest::Client,
    base_url: String,
}

impl ClaudeProvider {
    pub fn new(base_url: String) -> Self {
        // http1_only() sidesteps the reqwest+hyper HTTP/2 connection-pool stall
        // (reqwest#1323, #976, #1276) where a freshly-constructed client's
        // first request can hang in send().await indefinitely with no surface
        // error. Anthropic's API supports HTTP/1.1 fine.
        let client = reqwest::Client::builder()
            .http1_only()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client build");
        Self { client, base_url }
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
            unreachable!(
                "FileIncoming must be replaced by the controller before reaching the provider"
            )
        }
    }
}

fn build_api_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    // Anthropic's Messages API requires that when an assistant message
    // contains multiple parallel `tool_use` blocks, the immediately
    // following user message must contain ALL the corresponding
    // `tool_result` blocks in a single content array. Sending them as
    // separate user messages is accepted at the HTTP layer but produces
    // degraded model responses (empty TurnComplete events on the next
    // turn). Internally we keep one Message per tool result for easy
    // log writing; collapse consecutive `role: "tool"` entries here.
    let mut out: Vec<serde_json::Value> = Vec::with_capacity(messages.len());
    let mut i = 0;
    while i < messages.len() {
        let m = &messages[i];

        if m.role == "tool" {
            // Consume the maximal run of consecutive tool messages into
            // a single user message with multiple tool_result blocks.
            // Per Anthropic's docs (handling-stop-reasons) and reproduced
            // failure modes (opencode #15371), the `content` field of a
            // tool_result must be sent as an array of content blocks
            // rather than a bare string when the payload is non-trivial.
            // Sending strings here triggers empty `end_turn` responses
            // on the next model turn.
            let mut content_blocks: Vec<serde_json::Value> = Vec::new();
            while i < messages.len() && messages[i].role == "tool" {
                let tm = &messages[i];
                if let Some(ref tool_call_id) = tm.tool_call_id {
                    let text = content_text(&tm.content).unwrap_or("").to_string();
                    let mut block = serde_json::Map::new();
                    block.insert("type".into(), "tool_result".into());
                    block.insert("tool_use_id".into(), tool_call_id.clone().into());
                    block.insert(
                        "content".into(),
                        serde_json::json!([{"type": "text", "text": text}]),
                    );
                    if tm.is_error == Some(true) {
                        block.insert("is_error".into(), serde_json::Value::Bool(true));
                    }
                    content_blocks.push(serde_json::Value::Object(block));
                }
                i += 1;
            }
            // Anthropic rejects user messages with empty content arrays. If
            // every tool message in the run lacked a tool_call_id we'd emit
            // `content: []`; substitute a placeholder text block so the
            // tool_use ↔ tool_result pairing remains visible in logs.
            if content_blocks.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": "(empty tool result)",
                }));
            }
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), "user".into());
            obj.insert("content".into(), serde_json::Value::Array(content_blocks));
            out.push(serde_json::Value::Object(obj));
            continue;
        }

        let mut obj = serde_json::Map::new();
        if let Some(ref tool_calls) = m.tool_calls {
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
        out.push(serde_json::Value::Object(obj));
        i += 1;
    }
    out
}

fn build_api_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    let n = tools.len();
    tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut obj = serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            });
            // Anthropic walks back from a `cache_control` breakpoint
            // to cache every preceding tool entry; marking only the
            // last one caches the entire tools array.
            if i + 1 == n {
                if let Some(map) = obj.as_object_mut() {
                    map.insert(
                        "cache_control".into(),
                        serde_json::json!({"type": "ephemeral"}),
                    );
                }
            }
            obj
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
        params: Option<&Map<String, Value>>,
        config: &ProviderConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, String>> + Send>>, String> {
        let (body, clobbers) = build_anthropic_body(messages, system, tools, params, config);

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

        let warnings_stream = crate::merge::warning_stream_for(clobbers);
        let sse = parse_sse_stream(response);
        Ok(Box::pin(warnings_stream.chain(sse)))
    }

    fn managed_fields(&self) -> &'static [&'static str] {
        MANAGED_ANTHROPIC
    }
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
                if let Some((event_text, rest)) = crate::split_first_sse_event(&buffer) {
                    buffer = rest;
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
                "text" => {
                    // Plain-text content block. Anthropic emits a
                    // content_block_start for every text block, possibly
                    // with the initial text inline; subsequent text_delta
                    // events fill in the rest. Without this arm the
                    // start was silently dropped and any text the model
                    // emitted on turn 2 vanished — exactly the "1 events
                    // consumed, no content" symptom that broke our
                    // tool-use loop.
                    let initial_text = block
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(StreamEvent::ContentDelta { text: initial_text })
                }
                _ => None,
            }
        }
        "message_delta" => {
            let parsed: serde_json::Value = serde_json::from_str(&data).ok()?;
            let delta = parsed.get("delta")?;
            let stop_reason = delta.get("stop_reason")?.as_str()?.to_string();
            Some(StreamEvent::Done { stop_reason })
        }
        "message_start" => {
            // Surface cache usage from the `usage` block as structured fields
            // so observability backends (RUST_LOG, JSON logs, OTLP) can
            // track cache-hit ratio without scraping stderr.
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(usage) = parsed.get("message").and_then(|m| m.get("usage")) {
                    let cache_write = usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cache_read = usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let input_uncached = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    tracing::info!(cache_write, cache_read, input_uncached, "anthropic_usage");
                }
            }
            None
        }
        "message_stop" | "content_block_stop" | "ping" => None,
        _ => None,
    }
}

#[cfg(test)]
mod claude_body {
    use super::*;

    fn cfg() -> ProviderConfig {
        ProviderConfig {
            model: "claude-sonnet-4".into(),
            api_key: "sk-test".into(),
        }
    }

    #[test]
    fn body_inserts_max_tokens_default_when_absent() {
        let (body, _) = build_anthropic_body(&[], None, &[], None, &cfg());
        assert_eq!(
            body.get("max_tokens"),
            Some(&serde_json::Value::Number(8192.into()))
        );
    }

    #[test]
    fn body_max_tokens_passes_through_when_set_in_params() {
        let mut params = serde_json::Map::new();
        params.insert("max_tokens".into(), serde_json::json!(100000));
        let (body, _) = build_anthropic_body(&[], None, &[], Some(&params), &cfg());
        assert_eq!(
            body.get("max_tokens"),
            Some(&serde_json::Value::Number(100000.into()))
        );
    }

    #[test]
    fn body_clobbers_principal_messages_and_reports() {
        let mut params = serde_json::Map::new();
        params.insert("messages".into(), serde_json::json!(["forged"]));
        let (body, clobbers) = build_anthropic_body(&[], None, &[], Some(&params), &cfg());
        assert_eq!(clobbers, vec!["messages".to_string()]);
        // Sycophant's value (empty messages array) overwrites the principal's.
        assert_eq!(body.get("messages"), Some(&serde_json::json!([])));
    }

    #[test]
    fn body_passes_through_unmanaged_keys() {
        let mut params = serde_json::Map::new();
        params.insert(
            "output_config".into(),
            serde_json::json!({"effort": "high"}),
        );
        let (body, clobbers) = build_anthropic_body(&[], None, &[], Some(&params), &cfg());
        assert!(clobbers.is_empty());
        assert_eq!(
            body.get("output_config"),
            Some(&serde_json::json!({"effort": "high"}))
        );
    }

    #[test]
    fn body_omits_tools_when_empty() {
        let (body, _) = build_anthropic_body(&[], None, &[], None, &cfg());
        assert!(
            !body.contains_key("tools"),
            "tools field should not be set when no tools are provided"
        );
    }

    #[test]
    fn body_includes_tools_when_nonempty() {
        let tools = vec![ToolDefinition {
            name: "bash".into(),
            description: "shell".into(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let (body, _) = build_anthropic_body(&[], None, &tools, None, &cfg());
        assert!(body.contains_key("tools"));
    }

    #[test]
    fn system_emits_array_with_cache_control_breakpoint() {
        let (body, _) = build_anthropic_body(&[], Some("you are helpful"), &[], None, &cfg());
        let system = body.get("system").expect("system field present");
        let arr = system.as_array().expect("system is an array");
        assert_eq!(arr.len(), 1, "single text block expected");
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "you are helpful");
        assert_eq!(
            arr[0]["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn system_field_absent_when_no_system_passed() {
        let (body, _) = build_anthropic_body(&[], None, &[], None, &cfg());
        assert!(
            !body.contains_key("system"),
            "system field omitted when caller passes None"
        );
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
        }];
        let api = build_api_messages(&messages);
        assert_eq!(api[0]["role"], "user");
        let content = api[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "tc-1");
        // Anthropic accepts both string and array forms for
        // tool_result.content; we send array-of-blocks form to avoid
        // the empty-turn-2 regression triggered by string form with
        // non-trivial payloads.
        let result_content = content[0]["content"].as_array().unwrap();
        assert_eq!(result_content[0]["type"], "text");
        assert_eq!(result_content[0]["text"], "file list here");
    }

    /// Anthropic's Messages API requires that multiple parallel
    /// `tool_result` blocks live in a SINGLE user message — not several
    /// consecutive user messages. The previous implementation emitted
    /// one user message per tool result; this produced empty
    /// `TurnComplete` responses from Sonnet 4.6 on the next turn.
    #[test]
    fn consecutive_tool_results_collapse_into_one_user_message() {
        let messages = vec![
            Message {
                role: "tool".into(),
                content: Some(ContentBlock::text_content("result A")),
                tool_calls: None,
                tool_call_id: Some("tc-a".into()),
                is_error: None,
            },
            Message {
                role: "tool".into(),
                content: Some(ContentBlock::text_content("result B")),
                tool_calls: None,
                tool_call_id: Some("tc-b".into()),
                is_error: None,
            },
            Message {
                role: "tool".into(),
                content: Some(ContentBlock::text_content("result C")),
                tool_calls: None,
                tool_call_id: Some("tc-c".into()),
                is_error: None,
            },
        ];
        let api = build_api_messages(&messages);
        assert_eq!(api.len(), 1, "expected single user message; got {api:?}");
        assert_eq!(api[0]["role"], "user");
        let content = api[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        let ids: Vec<&str> = content
            .iter()
            .map(|b| b["tool_use_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["tc-a", "tc-b", "tc-c"]);
        // content is array-of-text-blocks form.
        let texts: Vec<&str> = content
            .iter()
            .map(|b| {
                b["content"].as_array().unwrap()[0]["text"]
                    .as_str()
                    .unwrap()
            })
            .collect();
        assert_eq!(texts, vec!["result A", "result B", "result C"]);
    }

    /// If every tool message in a run has no tool_call_id, the synthesized
    /// user message would otherwise have `content: []`, which Anthropic
    /// rejects. Substitute a placeholder text block.
    #[test]
    fn tool_results_with_no_tool_call_ids_emit_placeholder_text() {
        let messages = vec![
            Message {
                role: "tool".into(),
                content: Some(ContentBlock::text_content("dropped result")),
                tool_calls: None,
                tool_call_id: None,
                is_error: None,
            },
            Message {
                role: "tool".into(),
                content: Some(ContentBlock::text_content("also dropped")),
                tool_calls: None,
                tool_call_id: None,
                is_error: None,
            },
        ];
        let api = build_api_messages(&messages);
        assert_eq!(api.len(), 1);
        assert_eq!(api[0]["role"], "user");
        let content = api[0]["content"].as_array().unwrap();
        assert!(
            !content.is_empty(),
            "synthesized user content must not be empty"
        );
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "(empty tool result)");
    }

    /// is_error must round-trip into the Anthropic tool_result block.
    #[test]
    fn tool_result_is_error_propagates_to_block() {
        let messages = vec![Message {
            role: "tool".into(),
            content: Some(ContentBlock::text_content("error text")),
            tool_calls: None,
            tool_call_id: Some("tc-err".into()),
            is_error: Some(true),
        }];
        let api = build_api_messages(&messages);
        let content = api[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["is_error"], serde_json::Value::Bool(true));
    }

    /// A tool message followed by a non-tool message must not absorb
    /// the next message — the merge stops at the role boundary.
    #[test]
    fn tool_results_dont_swallow_following_assistant_message() {
        let messages = vec![
            Message {
                role: "tool".into(),
                content: Some(ContentBlock::text_content("result A")),
                tool_calls: None,
                tool_call_id: Some("tc-a".into()),
                is_error: None,
            },
            Message {
                role: "assistant".into(),
                content: Some(ContentBlock::text_content("ok continuing")),
                tool_calls: None,
                tool_call_id: None,
                is_error: None,
            },
        ];
        let api = build_api_messages(&messages);
        assert_eq!(api.len(), 2);
        assert_eq!(api[0]["role"], "user");
        assert_eq!(api[1]["role"], "assistant");
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

    #[test]
    fn build_api_tools_marks_only_last_tool_with_cache_control() {
        let tools = vec![
            ToolDefinition {
                name: "alpha".into(),
                description: "a".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolDefinition {
                name: "beta".into(),
                description: "b".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolDefinition {
                name: "gamma".into(),
                description: "g".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        ];
        let api = build_api_tools(&tools);
        assert!(
            api[0].get("cache_control").is_none(),
            "first tool must not carry cache_control"
        );
        assert!(
            api[1].get("cache_control").is_none(),
            "middle tool must not carry cache_control"
        );
        assert_eq!(
            api[2].get("cache_control"),
            Some(&serde_json::json!({"type": "ephemeral"})),
            "last tool carries the single breakpoint"
        );
    }

    #[test]
    fn build_api_tools_marks_sole_tool_when_only_one() {
        let tools = vec![ToolDefinition {
            name: "solo".into(),
            description: "the only tool".into(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let api = build_api_tools(&tools);
        assert_eq!(
            api[0].get("cache_control"),
            Some(&serde_json::json!({"type": "ephemeral"}))
        );
    }

    #[test]
    fn build_api_messages_does_not_attach_cache_control_to_messages() {
        // We mark `system` and `tools` (the static prefix) for caching,
        // but leave the messages tail untouched — putting a cache
        // breakpoint on a position that moves every turn appears to
        // destabilise the model's tool-use planning. See the
        // klein-wenner run-loop regression noted at swap time.
        let messages = vec![
            Message {
                role: "user".into(),
                content: Some(ContentBlock::text_content("hello")),
                tool_calls: None,
                tool_call_id: None,
                is_error: None,
            },
            Message {
                role: "assistant".into(),
                content: Some(ContentBlock::text_content("hi back")),
                tool_calls: None,
                tool_call_id: None,
                is_error: None,
            },
        ];
        let api = build_api_messages(&messages);
        for msg in &api {
            for block in msg["content"].as_array().expect("array") {
                assert!(
                    block.get("cache_control").is_none(),
                    "messages must not carry cache_control: {block:?}"
                );
            }
        }
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
    fn text_block_start_emits_initial_content_delta() {
        // Empty text block — emits ContentDelta{""} so the loop knows the
        // text channel opened. The accumulator that follows will append
        // any text_delta payload.
        let text = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}";
        let event = parse_sse_event(text).unwrap();
        match event {
            StreamEvent::ContentDelta { text } => assert_eq!(text, ""),
            _ => panic!("expected ContentDelta"),
        }
    }

    #[test]
    fn text_block_start_with_initial_text_captures_it() {
        let text = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Hello\"}}";
        let event = parse_sse_event(text).unwrap();
        match event {
            StreamEvent::ContentDelta { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected ContentDelta"),
        }
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
    fn thinking_delta_parses_into_thinking_event() {
        // Catches `delete match arm "thinking_delta"` — without the arm,
        // thinking_delta would fall through the inner match's `_ => None`.
        let text = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"reasoning step\"}}";
        let event = parse_sse_event(text).expect("thinking_delta should produce ThinkingDelta");
        match event {
            StreamEvent::ThinkingDelta { text } => assert_eq!(text, "reasoning step"),
            _ => panic!("expected ThinkingDelta"),
        }
    }

    #[test]
    fn content_block_start_thinking_parses_into_thinking_event() {
        // Catches `delete match arm "thinking"` — without it, a thinking
        // block-start would fall through to `_ => None`.
        let text = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}";
        let event =
            parse_sse_event(text).expect("thinking block_start should produce ThinkingDelta");
        match event {
            StreamEvent::ThinkingDelta { text } => assert_eq!(text, ""),
            _ => panic!("expected ThinkingDelta with empty text"),
        }
    }
}
