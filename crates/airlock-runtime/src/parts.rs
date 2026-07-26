//! Typed-frame assembly for tool answers.
//!
//! A dispatch script (or in-process builtin) returns an image by writing the
//! bytes to a scratch file in the chamber's own `/tmp` emptyDir and printing a
//! structured marker line on stdout that names the file and its media type. The
//! runtime turns a call's captured output into an ordered sequence of typed
//! frames: each marker line becomes a chamber-side `image` frame (its scratch
//! file read then deleted); every other stdout line becomes a scrubbed `stdout`
//! frame; every stderr line becomes a scrubbed `stderr` frame; one terminal
//! `ToolComplete` closes the stream. Secret scrubbing runs per text frame,
//! never over image bytes.

use airlock_proto::tool_result_frame::Frame;
use airlock_proto::{ToolComplete, ToolResultFrame};
use proto_common::content_block::Block;
use proto_common::{ContentBlock, ImageBlock};
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

/// Turn a tool call's captured `stdout`/`stderr` and `exit_code` into the
/// ordered typed-frame stream the runtime sends to its controller: image-marker
/// lines on stdout become chamber-side `image` frames; other stdout lines become
/// scrubbed `stdout` frames; stderr lines become scrubbed `stderr` frames; a
/// single terminal `ToolComplete` closes the stream. `scrub` is applied per text
/// frame before it leaves the chamber, so neither the model-facing result nor
/// the persisted execution log ever holds an unscrubbed secret. The terminal is
/// an error when the child exited non-zero or an image reference failed.
pub fn frames_for(
    stdout: &str,
    stderr: &str,
    exit_code: i32,
    scrub: &ScrubSet,
) -> Vec<ToolResultFrame> {
    let mut frames = Vec::new();
    let mut image_error = false;

    for piece in stdout.split_inclusive('\n') {
        let line = piece.strip_suffix('\n').unwrap_or(piece);
        let (frame, err) = stdout_line_frame(line, scrub);
        if err {
            image_error = true;
        }
        if let Some(f) = frame {
            frames.push(f);
        }
    }

    for piece in stderr.split_inclusive('\n') {
        let line = piece.strip_suffix('\n').unwrap_or(piece);
        frames.push(stderr_line_frame(line, scrub));
    }

    let is_error = exit_code != 0 || image_error;
    frames.push(complete_frame(is_error, exit_code));
    frames
}

/// Turn one raw stdout line into its typed frame, reusing the image-marker
/// convention. A marker line is assembled into an `image` frame (its scratch
/// file read then deleted); any other line becomes a scrubbed `stdout` frame.
/// The bool is `true` when a marker line failed to assemble (over-cap or
/// unreadable), so the caller marks the terminal an error. `None` is the
/// unreachable non-image `Ok` shape, which emits no frame. Shared by the
/// buffered [`frames_for`] and the incremental [`crate::execute::stream_frames`]
/// producer so both apply the marker/scrub rules identically per line.
pub(crate) fn stdout_line_frame(line: &str, scrub: &ScrubSet) -> (Option<ToolResultFrame>, bool) {
    match parse_image_marker(line) {
        Some(marker) => match read_scratch_image(&marker.path) {
            Ok(bytes) => match image_part_or_oversize_error(&marker.media_type, bytes) {
                Ok(ContentBlock {
                    block: Some(Block::Image(img)),
                }) => (Some(image_frame(img)), false),
                // `image_part_or_oversize_error` only ever returns an image part
                // on Ok; any other block shape is unreachable.
                Ok(_) => (None, false),
                Err(oversize) => (Some(stdout_frame(scrub.apply(&oversize.to_string()))), true),
            },
            Err(e) => (
                Some(stdout_frame(
                    scrub.apply(&format!("image result unavailable: {e}")),
                )),
                true,
            ),
        },
        None => (Some(stdout_frame(scrub.apply(line))), false),
    }
}

/// A single stderr line as its scrubbed `stderr` frame — never folded into
/// stdout.
pub(crate) fn stderr_line_frame(line: &str, scrub: &ScrubSet) -> ToolResultFrame {
    stderr_frame(scrub.apply(line))
}

/// The terminal `ToolComplete` frame that closes a tool call's stream.
pub(crate) fn complete_frame(is_error: bool, exit_code: i32) -> ToolResultFrame {
    ToolResultFrame {
        frame: Some(Frame::Complete(ToolComplete {
            is_error,
            exit_code,
        })),
    }
}

fn stdout_frame(text: String) -> ToolResultFrame {
    ToolResultFrame {
        frame: Some(Frame::Stdout(text)),
    }
}

fn stderr_frame(text: String) -> ToolResultFrame {
    ToolResultFrame {
        frame: Some(Frame::Stderr(text)),
    }
}

fn image_frame(image: ImageBlock) -> ToolResultFrame {
    ToolResultFrame {
        frame: Some(Frame::Image(image)),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn no_scrub() -> ScrubSet {
        ScrubSet::from_env_var("__UNSET_AIRLOCK_PARTS_TEST__")
    }

    fn marker(media_type: &str, path: &str) -> String {
        format!("{MARKER_PREFIX}{media_type}{FIELD_SEP}{path}")
    }

    fn variants(frames: &[ToolResultFrame]) -> Vec<&Frame> {
        frames.iter().filter_map(|f| f.frame.as_ref()).collect()
    }

    fn stdout_text(frames: &[ToolResultFrame]) -> String {
        variants(frames)
            .into_iter()
            .filter_map(|v| match v {
                Frame::Stdout(s) => Some(s.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn plain_stdout_rides_stdout_frames_then_a_terminal() {
        let frames = frames_for("hello world\n", "", 0, &no_scrub());
        assert_eq!(stdout_text(&frames), "hello world");
        match variants(&frames).last() {
            Some(Frame::Complete(c)) => {
                assert!(!c.is_error);
                assert_eq!(c.exit_code, 0);
            }
            other => panic!("last frame must be the terminal, got {other:?}"),
        }
    }

    #[test]
    fn marker_line_becomes_an_image_frame_and_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let bytes = vec![0x89u8, 0x50, 0x4e, 0x47, 1, 2, 3];
        std::fs::write(&path, &bytes).unwrap();

        let stdout = format!("{}\n", marker("image/png", path.to_str().unwrap()));
        let frames = frames_for(&stdout, "", 0, &no_scrub());
        let image = variants(&frames)
            .into_iter()
            .find_map(|v| match v {
                Frame::Image(img) => Some(img.clone()),
                _ => None,
            })
            .expect("a marker line produces an image frame");
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.data, bytes);
        assert!(
            !stdout_text(&frames).contains("AIRLOCK-IMAGE"),
            "the marker line is consumed into an image frame, not echoed as stdout"
        );
        assert!(!path.exists(), "the scratch file is removed after reading");
    }

    #[test]
    fn mixed_text_and_image_keeps_both_frame_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, [1u8, 2, 3]).unwrap();

        let stdout = format!(
            "caption line\n{}\n",
            marker("image/png", path.to_str().unwrap())
        );
        let frames = frames_for(&stdout, "", 0, &no_scrub());
        assert_eq!(stdout_text(&frames), "caption line");
        assert!(
            variants(&frames)
                .iter()
                .any(|v| matches!(v, Frame::Image(_))),
            "the marker still produces an image frame alongside the caption stdout frame"
        );
    }

    #[test]
    fn oversize_image_yields_an_error_terminal_not_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        std::fs::write(&path, vec![0u8; MAX_IMAGE_BYTES + 1]).unwrap();

        let stdout = format!("{}\n", marker("image/png", path.to_str().unwrap()));
        let frames = frames_for(&stdout, "", 0, &no_scrub());
        assert!(
            !variants(&frames)
                .iter()
                .any(|v| matches!(v, Frame::Image(_))),
            "no image frame is emitted for an over-cap image"
        );
        match variants(&frames).last() {
            Some(Frame::Complete(c)) => {
                assert!(c.is_error, "an over-cap image marks the terminal an error")
            }
            other => panic!("last frame must be the terminal, got {other:?}"),
        }
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
    fn stderr_rides_its_own_frame_never_folded_into_stdout() {
        let frames = frames_for("stdout line\n", "stderr detail\n", 0, &no_scrub());
        assert_eq!(stdout_text(&frames), "stdout line");
        let stderr: String = variants(&frames)
            .into_iter()
            .filter_map(|v| match v {
                Frame::Stderr(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(stderr, "stderr detail");
        assert!(
            !stdout_text(&frames).contains("stderr detail"),
            "stderr must never fold into a stdout frame"
        );
    }
}
