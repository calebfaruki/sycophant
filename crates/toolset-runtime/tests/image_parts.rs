//! Acceptance tests: tool-result media contract (chamber runtime side).
//!
//! The chamber runtime turns image bytes into an image content part and
//! enforces the 3.5 MiB cap by erroring rather than truncating. Both the
//! script-dispatch path and the in-process builtin path must route image
//! bytes through the same seam, so it lives as one function:
//!
//!     toolset_runtime::parts::image_part_or_oversize_error(media_type, bytes)
//!         -> Result<ContentBlock, _>   // Ok = image part; Err = over-cap.
//!
//! This is the runtime's enforcing seam for the cap; testing it here keeps the
//! cap load-bearing on the real path rather than on an unused helper.

use proto_common::content_block::Block;

/// 3.5 MiB, the spec's exact cap: 3,670,016 bytes.
const CAP: usize = 3_670_016;

// Materiality: the runtime must construct an image part from (media_type,
// bytes). A mutant that drops the media type, empties/truncates the data, or
// produces a text part instead reds the field assertions.
#[test]
fn image_under_cap_becomes_an_image_part_with_media_type_and_bytes() {
    let bytes = vec![0x89u8, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];
    let block = toolset_runtime::parts::image_part_or_oversize_error("image/png", bytes.clone())
        .expect("an under-cap image must produce an image part, never an error");
    match block.block {
        Some(Block::Image(img)) => {
            assert_eq!(img.media_type, "image/png", "media type is carried");
            assert_eq!(img.data, bytes, "the exact bytes are carried, untruncated");
        }
        other => panic!("expected an image part, got {other:?}"),
    }
}

// Boundary that pins the literal 3,670,016: an image of exactly the cap size
// is accepted (not rejected).
//
// Materiality: a mutant that uses `>=` instead of `>` (or a smaller literal)
// rejects the at-cap image and reds this.
#[test]
fn image_of_exactly_the_cap_size_is_accepted() {
    let bytes = vec![0u8; CAP];
    let block = toolset_runtime::parts::image_part_or_oversize_error("image/png", bytes)
        .expect("an image of exactly 3,670,016 bytes is within the cap");
    assert!(
        matches!(block.block, Some(Block::Image(_))),
        "an at-cap image is still an image part"
    );
}

// One byte over the cap must be an error — never any image part (truncated or
// whole).
//
// Materiality: a mutant that drops the guard (always Ok), or truncates the
// bytes to the cap and returns an image, reds this `is_err()`. The larger
// literal (or flipped comparison) that admits the over-cap image also reds it.
#[test]
fn image_one_byte_over_the_cap_returns_an_error_not_a_truncated_image() {
    let bytes = vec![0u8; CAP + 1];
    let result = toolset_runtime::parts::image_part_or_oversize_error("image/png", bytes);
    assert!(
        result.is_err(),
        "an over-cap image must return an error rather than any image part"
    );
}
