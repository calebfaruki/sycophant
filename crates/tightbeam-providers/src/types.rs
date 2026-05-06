use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "file_incoming")]
    FileIncoming {
        filename: String,
        mime_type: String,
        size: u64,
    },
    #[serde(rename = "image")]
    Image { media_type: String, data: String },
    #[serde(rename = "thinking")]
    Thinking { text: String },
}

impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }

    pub fn text_content(s: impl Into<String>) -> Vec<ContentBlock> {
        vec![Self::text(s)]
    }

    pub fn file_incoming(
        filename: impl Into<String>,
        mime_type: impl Into<String>,
        size: u64,
    ) -> Self {
        Self::FileIncoming {
            filename: filename.into(),
            mime_type: mime_type.into(),
            size,
        }
    }

    pub fn thinking(s: impl Into<String>) -> Self {
        Self::Thinking { text: s.into() }
    }

    pub fn image(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            media_type: media_type.into(),
            data: data.into(),
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }
}

pub fn file_incoming_indices(blocks: &[ContentBlock]) -> Vec<usize> {
    blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| matches!(b, ContentBlock::FileIncoming { .. }).then_some(i))
        .collect()
}

pub fn is_supported_image(mime_type: &str) -> bool {
    matches!(
        mime_type.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

pub fn content_text(blocks: &Option<Vec<ContentBlock>>) -> Option<&str> {
    blocks
        .as_ref()
        .and_then(|b| b.first())
        .and_then(|b| b.as_text())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
}

impl StopReason {
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "end_turn" => Self::EndTurn,
            "tool_use" => Self::ToolUse,
            "max_tokens" => Self::MaxTokens,
            _ => Self::EndTurn,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnResponse {
    pub stop_reason: StopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_text_serializes() {
        let block = ContentBlock::text("hello");
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(json, r#"{"type":"text","text":"hello"}"#);

        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_text(), Some("hello"));
    }

    #[test]
    fn content_text_helper() {
        let blocks = Some(ContentBlock::text_content("hello"));
        assert_eq!(content_text(&blocks), Some("hello"));

        let none: Option<Vec<ContentBlock>> = None;
        assert_eq!(content_text(&none), None);

        let empty: Option<Vec<ContentBlock>> = Some(vec![]);
        assert_eq!(content_text(&empty), None);
    }

    #[test]
    fn message_round_trips() {
        let msg = Message {
            role: "user".into(),
            content: Some(ContentBlock::text_content("hello")),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("tool_calls"));
        assert!(!json.contains("is_error"));

        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.role, "user");
        assert_eq!(content_text(&parsed.content), Some("hello"));
    }

    #[test]
    fn message_with_tool_calls_round_trips() {
        let msg = Message {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc-1".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
            }]),
            tool_call_id: None,
            is_error: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();
        let tc = parsed.tool_calls.unwrap();
        assert_eq!(tc[0].name, "bash");
    }

    #[test]
    fn stop_reason_serializes_to_snake_case() {
        assert_eq!(
            serde_json::to_string(&StopReason::EndTurn).unwrap(),
            "\"end_turn\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::ToolUse).unwrap(),
            "\"tool_use\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::MaxTokens).unwrap(),
            "\"max_tokens\""
        );
    }

    #[test]
    fn stop_reason_from_str_lossy() {
        assert!(matches!(
            StopReason::from_str_lossy("end_turn"),
            StopReason::EndTurn
        ));
        assert!(matches!(
            StopReason::from_str_lossy("tool_use"),
            StopReason::ToolUse
        ));
        assert!(matches!(
            StopReason::from_str_lossy("max_tokens"),
            StopReason::MaxTokens
        ));
        assert!(matches!(
            StopReason::from_str_lossy("unknown"),
            StopReason::EndTurn
        ));
    }

    #[test]
    fn turn_request_omits_none_fields() {
        let req = TurnRequest {
            system: None,
            tools: None,
            messages: vec![],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"system\""));
        assert!(!json.contains("\"tools\""));
    }

    #[test]
    fn file_incoming_round_trips() {
        let block = ContentBlock::file_incoming("photo.png", "image/png", 1024);
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""type":"file_incoming""#));
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, block);
    }

    #[test]
    fn image_round_trips() {
        let block = ContentBlock::image("image/png", "iVBOR...");
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""type":"image""#));
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, block);
    }

    #[test]
    fn is_supported_image_accepts_valid_types() {
        assert!(is_supported_image("image/png"));
        assert!(is_supported_image("image/jpeg"));
        assert!(is_supported_image("image/gif"));
        assert!(is_supported_image("image/webp"));
    }

    #[test]
    fn is_supported_image_rejects_non_images() {
        assert!(!is_supported_image("application/pdf"));
        assert!(!is_supported_image("image/svg+xml"));
    }

    #[test]
    fn plain_string_content_rejected() {
        let json = r#"{"role":"user","content":"plain string"}"#;
        let result: Result<Message, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn thinking_block_round_trips() {
        let block = ContentBlock::thinking("deep thoughts");
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, block);
    }

    #[test]
    fn file_incoming_indices_finds_correct_positions() {
        let blocks = vec![
            ContentBlock::text("hello"),
            ContentBlock::file_incoming("a.png", "image/png", 100),
            ContentBlock::text("world"),
            ContentBlock::file_incoming("b.jpg", "image/jpeg", 200),
        ];
        assert_eq!(file_incoming_indices(&blocks), vec![1, 3]);
    }
}
