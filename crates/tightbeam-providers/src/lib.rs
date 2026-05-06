pub mod claude;
pub mod gemini;
pub mod merge;
pub mod openai;
pub mod types;

pub use types::*;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

/// Split the first complete SSE event off the front of an accumulating buffer.
///
/// Returns `Some((event_text, remaining_buffer))` if a `\n\n` separator is found.
/// Returns `None` if the buffer doesn't yet contain a complete event.
///
/// Shared by `claude::parse_sse_stream` and `openai::parse_sse_stream`. The
/// `+ 2` offset to skip past the separator is the load-bearing arithmetic;
/// extracted for unit-testability.
pub(crate) fn split_first_sse_event(buffer: &str) -> Option<(String, String)> {
    let pos = buffer.find("\n\n")?;
    Some((buffer[..pos].to_string(), buffer[pos + 2..].to_string()))
}

#[cfg(test)]
mod sse_buffer_tests {
    use super::*;

    #[test]
    fn split_returns_none_when_no_separator() {
        assert_eq!(split_first_sse_event("event: foo\ndata: bar"), None);
    }

    #[test]
    fn split_returns_event_and_remainder_at_first_separator() {
        let (event, rest) = split_first_sse_event("first\n\nsecond\n\nthird").unwrap();
        assert_eq!(event, "first");
        assert_eq!(rest, "second\n\nthird");
    }

    #[test]
    fn split_with_two_events_extracted_in_sequence() {
        // Catches `pos + 2 -> pos - 2` and `pos + 2 -> pos * 2` mutations.
        // If the offset is wrong, the second event would be malformed.
        let (event1, rest) = split_first_sse_event("a\n\nb\n\nc").unwrap();
        assert_eq!(event1, "a");
        let (event2, rest2) = split_first_sse_event(&rest).unwrap();
        assert_eq!(event2, "b");
        assert_eq!(rest2, "c");
    }

    #[test]
    fn split_with_empty_event_text() {
        let (event, rest) = split_first_sse_event("\n\nremainder").unwrap();
        assert_eq!(event, "");
        assert_eq!(rest, "remainder");
    }
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    ContentDelta { text: String },
    ThinkingDelta { text: String },
    ToolUseStart { id: String, name: String },
    ToolUseInput { json: String },
    Done { stop_reason: String },
    Warning { field: String, reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Anthropic,
    #[serde(rename = "openai")]
    OpenAi,
    Gemini,
}

impl Format {
    pub fn build(&self, base_url: &str) -> Box<dyn LlmProvider> {
        match self {
            Self::Anthropic => Box::new(claude::ClaudeProvider::new(base_url.to_string())),
            Self::OpenAi => Box::new(openai::OpenAiProvider::new(base_url.to_string())),
            Self::Gemini => Box::new(gemini::GeminiProvider::new(base_url.to_string())),
        }
    }
}

pub struct ProviderConfig {
    pub model: String,
    pub api_key: String,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn call(
        &self,
        messages: &[Message],
        system: Option<&str>,
        tools: &[ToolDefinition],
        params: Option<&serde_json::Map<String, serde_json::Value>>,
        config: &ProviderConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, String>> + Send>>, String>;

    fn managed_fields(&self) -> &'static [&'static str];
}

pub fn collect_tool_calls(events: &[StreamEvent]) -> Vec<ToolCall> {
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for event in events {
        match event {
            StreamEvent::ToolUseStart { id, name } => {
                tool_calls.push(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: serde_json::Value::Null,
                });
            }
            StreamEvent::ToolUseInput { json } => {
                if let Some(tc) = tool_calls.last_mut() {
                    let existing = match &tc.input {
                        serde_json::Value::Null => String::new(),
                        serde_json::Value::String(s) => s.clone(),
                        _ => serde_json::to_string(&tc.input).unwrap_or_default(),
                    };
                    let combined = format!("{existing}{json}");
                    tc.input = serde_json::Value::String(combined);
                }
            }
            StreamEvent::Done { .. } => {
                for tc in &mut tool_calls {
                    if let serde_json::Value::String(s) = &tc.input {
                        if let Ok(parsed) = serde_json::from_str(s) {
                            tc.input = parsed;
                        }
                    }
                }
            }
            StreamEvent::ContentDelta { .. }
            | StreamEvent::ThinkingDelta { .. }
            | StreamEvent::Warning { .. } => {}
        }
    }

    tool_calls
}

pub fn collect_text(events: &[StreamEvent]) -> Option<String> {
    let mut text = String::new();
    for event in events {
        if let StreamEvent::ContentDelta { text: t } = event {
            text.push_str(t);
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

pub fn collect_thinking(events: &[StreamEvent]) -> Option<String> {
    let mut text = String::new();
    for event in events {
        if let StreamEvent::ThinkingDelta { text: t } = event {
            text.push_str(t);
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod provider_helpers {
    use super::*;

    #[test]
    fn collect_text_from_deltas() {
        let events = vec![
            StreamEvent::ContentDelta {
                text: "Hello ".into(),
            },
            StreamEvent::ContentDelta {
                text: "world".into(),
            },
        ];
        assert_eq!(collect_text(&events), Some("Hello world".into()));
    }

    #[test]
    fn collect_text_empty_when_no_deltas() {
        let events = vec![StreamEvent::Done {
            stop_reason: "end_turn".into(),
        }];
        assert_eq!(collect_text(&events), None);
    }

    #[test]
    fn collect_tool_calls_assembles_from_events() {
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "tc-1".into(),
                name: "bash".into(),
            },
            StreamEvent::ToolUseInput {
                json: r#"{"comm"#.into(),
            },
            StreamEvent::ToolUseInput {
                json: r#"and":"ls"}"#.into(),
            },
            StreamEvent::Done {
                stop_reason: "tool_use".into(),
            },
        ];
        let calls = collect_tool_calls(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "tc-1");
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].input, serde_json::json!({"command": "ls"}));
    }

    #[test]
    fn collect_tool_calls_handles_multiple() {
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "tc-1".into(),
                name: "bash".into(),
            },
            StreamEvent::ToolUseInput {
                json: r#"{"command":"ls"}"#.into(),
            },
            StreamEvent::ToolUseStart {
                id: "tc-2".into(),
                name: "read".into(),
            },
            StreamEvent::ToolUseInput {
                json: r#"{"path":"foo.rs"}"#.into(),
            },
            StreamEvent::Done {
                stop_reason: "tool_use".into(),
            },
        ];
        let calls = collect_tool_calls(&events);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[1].name, "read");
    }

    #[test]
    fn collect_tool_calls_keeps_raw_string_on_invalid_json() {
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "tc-1".into(),
                name: "bash".into(),
            },
            StreamEvent::ToolUseInput {
                json: "not valid json{".into(),
            },
            StreamEvent::Done {
                stop_reason: "tool_use".into(),
            },
        ];
        let calls = collect_tool_calls(&events);
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].input.is_string(),
            "input should stay as raw string when JSON parsing fails"
        );
    }

    #[test]
    fn format_enum_deserializes_from_lowercase() {
        let f: Format = serde_json::from_str("\"anthropic\"").unwrap();
        assert_eq!(f, Format::Anthropic);
        let f: Format = serde_json::from_str("\"openai\"").unwrap();
        assert_eq!(f, Format::OpenAi);
    }

    #[test]
    fn format_enum_deserializes_gemini() {
        let f: Format = serde_json::from_str("\"gemini\"").unwrap();
        assert_eq!(f, Format::Gemini);
    }

    #[test]
    fn format_enum_rejects_unknown() {
        let result: Result<Format, _> = serde_json::from_str("\"banana\"");
        assert!(result.is_err());
    }

    #[test]
    fn claude_managed_fields_returns_anthropic_keys() {
        let provider = Format::Anthropic.build("https://api.anthropic.com/v1");
        assert_eq!(
            provider.managed_fields(),
            &["model", "messages", "system", "tools", "stream"]
        );
    }

    #[test]
    fn openai_managed_fields_returns_chat_completion_keys() {
        let provider = Format::OpenAi.build("https://api.openai.com/v1");
        assert_eq!(
            provider.managed_fields(),
            &["model", "messages", "tools", "stream"]
        );
    }

    #[test]
    fn gemini_managed_fields_returns_expected_keys() {
        let provider = Format::Gemini.build("https://generativelanguage.googleapis.com");
        assert_eq!(
            provider.managed_fields(),
            &["model", "contents", "systemInstruction", "tools"]
        );
    }

    #[test]
    fn collect_thinking_from_deltas() {
        let events = vec![
            StreamEvent::ThinkingDelta {
                text: "Let me ".into(),
            },
            StreamEvent::ThinkingDelta {
                text: "think...".into(),
            },
        ];
        assert_eq!(collect_thinking(&events), Some("Let me think...".into()));
    }

    #[test]
    fn collect_thinking_none_when_no_deltas() {
        let events = vec![StreamEvent::ContentDelta {
            text: "hello".into(),
        }];
        assert_eq!(collect_thinking(&events), None);
    }
}
