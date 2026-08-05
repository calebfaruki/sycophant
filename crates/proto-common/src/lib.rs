pub mod sycophant {
    pub mod common {
        pub mod v1 {
            tonic::include_proto!("sycophant.common.v1");
        }
    }
}

pub use sycophant::common::v1::*;

/// Build a text content part. Every tool producer that answers with plain text
/// wraps its string here so a text answer is a one-element content-part list.
pub fn text_block(text: String) -> ContentBlock {
    ContentBlock {
        block: Some(content_block::Block::Text(TextBlock { text })),
    }
}

/// Build an image content part carrying a media type (e.g. `image/png`) and the
/// raw bytes. The only picture representation in the system.
pub fn image_block(media_type: String, data: Vec<u8>) -> ContentBlock {
    ContentBlock {
        block: Some(content_block::Block::Image(ImageBlock { media_type, data })),
    }
}

/// Build a content part carrying an incoming-file reference (filename, MIME
/// type, byte size). The single representation of a file attachment in the
/// message vocabulary.
pub fn file_block(filename: String, mime_type: String, size: u64) -> ContentBlock {
    ContentBlock {
        block: Some(content_block::Block::File(FileBlock {
            filename,
            mime_type,
            size,
        })),
    }
}

/// Build a one-element content-part list holding a single text part — the shape
/// a text-only tool answer takes on the wire. The build-side mirror of
/// [`content_text`].
pub fn text_content(s: &str) -> Vec<ContentBlock> {
    vec![text_block(s.to_string())]
}

/// Join the text of a content-part list's text parts, separated by newlines.
/// Image parts are skipped. This is the one text read every consumer performs
/// on a tool answer.
pub fn content_text(parts: &[ContentBlock]) -> String {
    parts
        .iter()
        .filter_map(|b| match b.block.as_ref() {
            Some(content_block::Block::Text(t)) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_with_file_block_serializes_and_round_trips() {
        let msg = Message {
            role: "user".into(),
            content: vec![
                text_block("see attached".into()),
                file_block("photo.png".into(), "image/png".into(), 1024),
            ],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.role, "user");
        assert_eq!(back.content.len(), 2);
        match back.content[1].block.as_ref().unwrap() {
            content_block::Block::File(f) => {
                assert_eq!(f.filename, "photo.png");
                assert_eq!(f.mime_type, "image/png");
                assert_eq!(f.size, 1024);
            }
            other => panic!("expected FileBlock, got {other:?}"),
        }
    }
}
