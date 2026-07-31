//! The runtime turns a tool call's captured output into typed frames.
//!
//! The captured stdout/stderr become an ordered sequence of typed frames —
//! stdout / stderr / image, terminated by one ToolComplete — rather than a
//! single buffered result. Image markers are assembled chamber-side into image
//! frames; stdout and stderr each ride their own frame kind; every text frame is
//! scrubbed before it leaves the chamber.

use airlock_runtime::parts::frames_for;
use proto_common::tool_result_frame::Frame;
use proto_common::ToolOutcome;
use serial_test::serial;
use shared::scrub::ScrubSet;

const US: char = '\u{1f}';

fn no_scrub() -> ScrubSet {
    ScrubSet::from_env_var("__UNSET_FRAMES_TEST_SCRUB__")
}

/// The image-marker line grammar the chamber emits on stdout.
fn marker(media_type: &str, path: &str) -> String {
    format!("{US}AIRLOCK-IMAGE{US}{media_type}{US}{path}")
}

fn variants(frames: &[proto_common::ToolResultFrame]) -> Vec<&Frame> {
    frames.iter().filter_map(|f| f.frame.as_ref()).collect()
}

fn stdout_text(vars: &[&Frame]) -> String {
    vars.iter()
        .filter_map(|v| match v {
            Frame::Stdout(s) => Some(s.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn stderr_text(vars: &[&Frame]) -> String {
    vars.iter()
        .filter_map(|v| match v {
            Frame::Stderr(s) => Some(s.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// A tool call's output streams as an ordered sequence of TYPED frames — stdout
// and stderr on distinct frame kinds — terminating in one ToolComplete, not a
// single buffered result.
//
// Materiality: the stub returns no frames, reding every assertion. A mutant
// that folds stderr into a stdout frame (today's behavior) reds the
// "stderr not in a stdout frame" assertion; dropping the terminal ToolComplete
// reds the last-frame assertion; a wrong terminal exit_code reds it too.
#[test]
fn tool_output_streams_as_ordered_typed_frames_ending_in_tool_complete() {
    let frames = frames_for("first line\nsecond line\n", "a warning\n", 0, &no_scrub());
    let vars = variants(&frames);

    let out = stdout_text(&vars);
    assert!(
        out.contains("first line") && out.contains("second line"),
        "stdout lines ride stdout frames, got {out:?}"
    );

    let err = stderr_text(&vars);
    assert!(
        err.contains("a warning"),
        "stderr rides its own stderr frame, got {err:?}"
    );
    assert!(
        !out.contains("a warning"),
        "stderr must be a distinct frame kind, never folded into a stdout frame"
    );

    match vars.last() {
        Some(Frame::Complete(c)) => {
            assert_eq!(
                c.outcome(),
                ToolOutcome::Done,
                "a zero-exit call's terminal is not an error"
            );
            assert_eq!(c.exit_code, 0, "terminal carries the child's exit code");
        }
        other => panic!("the last frame must be the terminal ToolComplete, got {other:?}"),
    }
}

// An image result is assembled chamber-side and emitted as an image frame, not
// sent as raw bytes on a text frame. The scratch file is consumed.
//
// Materiality: the stub emits no frames, reding the image-frame lookup. A
// mutant that passes the marker line through as a stdout text frame (rather
// than reading+assembling the scratch image) reds the image-frame assertion; a
// mutant that stops deleting the scratch file reds the deletion assertion.
#[test]
fn an_image_marker_becomes_a_chamber_side_image_frame_and_consumes_the_scratch_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");
    let bytes = vec![0x89u8, 0x50, 0x4e, 0x47, 1, 2, 3];
    std::fs::write(&path, &bytes).unwrap();

    let stdout = format!("{}\n", marker("image/png", path.to_str().unwrap()));
    let frames = frames_for(&stdout, "", 0, &no_scrub());
    let vars = variants(&frames);

    let image = vars.iter().find_map(|v| match v {
        Frame::Image(img) => Some(img),
        _ => None,
    });
    let image = image.expect("a marker line must produce an assembled image frame");
    assert_eq!(
        image.media_type, "image/png",
        "media type carried on the frame"
    );
    assert_eq!(
        image.data, bytes,
        "the assembled image bytes ride the frame"
    );

    // No raw image bytes leak onto a stdout text frame.
    assert!(
        !stdout_text(&vars).contains("AIRLOCK-IMAGE"),
        "the marker line is consumed into an image frame, not echoed as stdout text"
    );
    assert!(
        !path.exists(),
        "the chamber scratch image is deleted after assembly"
    );
}

// stdout and stderr frames are scrubbed of secret values before they cross the
// gRPC boundary, so neither the model-facing result nor the execution log ever
// holds an unscrubbed secret.
//
// Materiality: the stub emits no frames, reding the redaction-tag assertion. A
// mutant that scrubs the stdout frame but not the stderr frame leaves the
// secret in the stderr frame, reding the per-frame leak assertion; dropping
// scrub entirely reds both.
#[test]
#[serial]
fn stdout_and_stderr_frames_are_scrubbed_before_leaving_the_chamber() {
    std::env::set_var("FRAMES_TEST_SECRET_VAL", "hunter2-secret-token");
    std::env::set_var(
        "FRAMES_TEST_SCRUB",
        r#"[{"name":"Tok","env":"FRAMES_TEST_SECRET_VAL"}]"#,
    );
    let scrub = ScrubSet::from_env_var("FRAMES_TEST_SCRUB");
    assert!(
        !scrub.is_empty(),
        "test setup: the scrub set must be armed with the secret"
    );

    let frames = frames_for(
        "stdout mentions hunter2-secret-token here\n",
        "stderr mentions hunter2-secret-token too\n",
        0,
        &scrub,
    );

    std::env::remove_var("FRAMES_TEST_SCRUB");
    std::env::remove_var("FRAMES_TEST_SECRET_VAL");

    let vars = variants(&frames);
    for v in &vars {
        if let Frame::Stdout(s) | Frame::Stderr(s) = v {
            assert!(
                !s.contains("hunter2-secret-token"),
                "every text frame must be scrubbed; secret leaked in {s:?}"
            );
        }
    }
    let joined = format!("{}|{}", stdout_text(&vars), stderr_text(&vars));
    assert!(
        joined.contains("[REDACTED:Tok]"),
        "scrubbed frames carry the redaction tag (proves scrub ran, not that lines were dropped), got {joined:?}"
    );
}
