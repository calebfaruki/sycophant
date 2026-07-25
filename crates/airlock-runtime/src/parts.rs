//! Content-part assembly for tool answers.
//!
//! A dispatch script (or in-process builtin) returns an image by writing the
//! bytes to a scratch file in the chamber's own `/tmp` emptyDir and printing a
//! structured marker line on stdout that names the file and its media type. The
//! runtime splits marker lines from plain text: each marker reads its file into
//! an image part (then deletes it); the un-marked stdout stays a single text
//! part. Secret scrubbing runs over the text part only, never over image bytes.

use proto_common::ContentBlock;
use shared::scrub::ScrubSet;

/// Tool-answer image cap: 3.5 MiB. Stays under tonic's 4 MiB decode limit on
/// the internal Rust hops the answer crosses. A larger image is rejected with
/// an error rather than truncated.
pub const MAX_IMAGE_BYTES: usize = 3_670_016;

/// Sentinel prefixing a stdout line that references an image scratch file
/// rather than carrying tool text. The ASCII unit-separator (`0x1F`)
/// delimiters cannot occur in ordinary printable tool output, so a marker
/// cannot collide with a tool's own text, and scrubbing (which replaces secret
/// substrings in text) leaves it untouched. Grammar of a marker line:
///
/// ```text
/// <US>AIRLOCK-IMAGE<US>{media_type}<US>{absolute_path}
/// ```
const MARKER_PREFIX: &str = "\u{1f}AIRLOCK-IMAGE\u{1f}";
const FIELD_SEP: char = '\u{1f}';

/// A parsed image marker: which scratch file to read and its media type.
struct ImageMarker {
    media_type: String,
    path: String,
}

/// The assembled tool answer: its content parts, plus whether an image
/// reference failed the cap or could not be read (so the answer carries an
/// error text part in place of that image).
pub struct Assembled {
    pub content: Vec<ContentBlock>,
    pub image_error: bool,
}

/// Over-cap image: carries the offending length. Returned by
/// [`image_part_or_oversize_error`] rather than any (whole or truncated) image
/// part.
#[derive(Debug)]
pub struct OversizeImage {
    pub len: usize,
}

impl std::fmt::Display for OversizeImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "image result is {} bytes, over the {}-byte (3.5 MiB) cap",
            self.len, MAX_IMAGE_BYTES
        )
    }
}

impl std::error::Error for OversizeImage {}

/// Build an image content part from `(media_type, bytes)`, or an error when the
/// bytes exceed the 3.5 MiB cap — an over-cap image returns an error, never a
/// truncated image part.
pub fn image_part_or_oversize_error(
    media_type: &str,
    bytes: Vec<u8>,
) -> Result<ContentBlock, OversizeImage> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(OversizeImage { len: bytes.len() });
    }
    Ok(proto_common::image_block(media_type.to_string(), bytes))
}

fn parse_image_marker(line: &str) -> Option<ImageMarker> {
    let rest = line.strip_prefix(MARKER_PREFIX)?;
    let mut fields = rest.splitn(2, FIELD_SEP);
    let media_type = fields.next()?.to_string();
    let path = fields.next()?.to_string();
    if media_type.is_empty() || path.is_empty() {
        return None;
    }
    Some(ImageMarker { media_type, path })
}

/// Read a scratch image file and remove it. The bytes never ride the text
/// stream and the file lives in the chamber's own ephemeral filesystem.
fn read_scratch_image(path: &str) -> std::io::Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    let _ = std::fs::remove_file(path);
    Ok(bytes)
}

/// Assemble a tool answer's content parts from a dispatch's captured output.
/// Marker lines on `stdout` become image parts (their scratch files read and
/// deleted); the remaining un-marked stdout, with `stderr` appended, becomes a
/// single text part. `scrub` is applied to that text part only. A pure-text
/// answer is exactly one text part; a pure-image answer carries no empty text
/// part.
pub fn assemble_tool_answer(stdout: &str, stderr: &str, scrub: &ScrubSet) -> Assembled {
    let mut images = Vec::new();
    let mut text = String::new();
    let mut image_error = false;

    for piece in stdout.split_inclusive('\n') {
        let line = piece.strip_suffix('\n').unwrap_or(piece);
        match parse_image_marker(line) {
            Some(marker) => match read_scratch_image(&marker.path) {
                Ok(bytes) => match image_part_or_oversize_error(&marker.media_type, bytes) {
                    Ok(part) => images.push(part),
                    Err(oversize) => {
                        image_error = true;
                        text.push_str(&oversize.to_string());
                        text.push('\n');
                    }
                },
                Err(e) => {
                    image_error = true;
                    text.push_str(&format!("image result unavailable: {e}\n"));
                }
            },
            None => text.push_str(piece),
        }
    }

    if !stderr.is_empty() {
        text.push_str(stderr);
    }

    let text = scrub.apply(&text);

    let mut content = images;
    // Un-marked stdout (plus stderr) stays a single text part. A pure-image
    // answer with no text carries no empty text part.
    if content.is_empty() || !text.is_empty() {
        content.push(proto_common::text_block(text));
    }

    Assembled {
        content,
        image_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto_common::content_block::Block;

    fn no_scrub() -> ScrubSet {
        ScrubSet::from_env_var("__UNSET_AIRLOCK_PARTS_TEST__")
    }

    fn marker(media_type: &str, path: &str) -> String {
        format!("{MARKER_PREFIX}{media_type}{FIELD_SEP}{path}")
    }

    #[test]
    fn plain_stdout_is_a_single_text_part() {
        let a = assemble_tool_answer("hello world\n", "", &no_scrub());
        assert!(!a.image_error);
        assert_eq!(a.content.len(), 1);
        match a.content[0].block.as_ref() {
            Some(Block::Text(t)) => assert_eq!(t.text, "hello world\n"),
            other => panic!("expected a text part, got {other:?}"),
        }
    }

    #[test]
    fn marker_line_becomes_an_image_part_and_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let bytes = vec![0x89u8, 0x50, 0x4e, 0x47, 1, 2, 3];
        std::fs::write(&path, &bytes).unwrap();

        let stdout = marker("image/png", path.to_str().unwrap());
        let a = assemble_tool_answer(&stdout, "", &no_scrub());
        assert!(!a.image_error);
        assert_eq!(a.content.len(), 1, "a pure-image answer is just the image");
        match a.content[0].block.as_ref() {
            Some(Block::Image(img)) => {
                assert_eq!(img.media_type, "image/png");
                assert_eq!(img.data, bytes);
            }
            other => panic!("expected an image part, got {other:?}"),
        }
        assert!(!path.exists(), "the scratch file is removed after reading");
    }

    #[test]
    fn mixed_text_and_image_keeps_both_parts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, [1u8, 2, 3]).unwrap();

        let stdout = format!(
            "caption line\n{}\n",
            marker("image/png", path.to_str().unwrap())
        );
        let a = assemble_tool_answer(&stdout, "", &no_scrub());
        assert_eq!(a.content.len(), 2);
        assert!(matches!(a.content[0].block.as_ref(), Some(Block::Image(_))));
        match a.content[1].block.as_ref() {
            Some(Block::Text(t)) => assert_eq!(t.text, "caption line\n"),
            other => panic!("expected a text part, got {other:?}"),
        }
    }

    #[test]
    fn oversize_image_yields_an_error_not_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        std::fs::write(&path, vec![0u8; MAX_IMAGE_BYTES + 1]).unwrap();

        let stdout = marker("image/png", path.to_str().unwrap());
        let a = assemble_tool_answer(&stdout, "", &no_scrub());
        assert!(a.image_error, "an over-cap image marks the answer an error");
        assert!(
            a.content
                .iter()
                .all(|b| !matches!(b.block.as_ref(), Some(Block::Image(_)))),
            "no image part is emitted for an over-cap image"
        );
    }

    #[test]
    fn marker_with_one_empty_field_is_not_a_valid_image() {
        // A marker is valid only when BOTH fields are present. One empty field —
        // a blank media type or a blank path — must be rejected, not accepted.
        assert!(
            parse_image_marker(&marker("", "/tmp/shot.png")).is_none(),
            "a blank media type is not a valid marker"
        );
        assert!(
            parse_image_marker(&marker("image/png", "")).is_none(),
            "a blank path is not a valid marker"
        );
    }

    #[test]
    fn non_empty_stderr_is_appended_to_the_text_part() {
        let a = assemble_tool_answer("stdout line\n", "stderr detail", &no_scrub());
        match a.content.last().and_then(|b| b.block.as_ref()) {
            Some(Block::Text(t)) => {
                assert!(t.text.contains("stdout line"));
                assert!(
                    t.text.contains("stderr detail"),
                    "non-empty stderr must be appended to the text part"
                );
            }
            other => panic!("expected a text part, got {other:?}"),
        }
    }
}
