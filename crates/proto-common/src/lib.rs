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
