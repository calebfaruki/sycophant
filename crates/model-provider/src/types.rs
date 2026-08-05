use proto_common::{content_block, ContentBlock};

pub fn file_incoming_indices(blocks: &[ContentBlock]) -> Vec<usize> {
    blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| matches!(&b.block, Some(content_block::Block::File(_))).then_some(i))
        .collect()
}

pub fn is_supported_image(mime_type: &str) -> bool {
    matches!(
        mime_type.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto_common::{file_block, text_block};

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
    fn file_incoming_indices_finds_correct_positions() {
        let blocks = vec![
            text_block("hello".into()),
            file_block("a.png".into(), "image/png".into(), 100),
            text_block("world".into()),
            file_block("b.jpg".into(), "image/jpeg".into(), 200),
        ];
        assert_eq!(file_incoming_indices(&blocks), vec![1, 3]);
    }
}
