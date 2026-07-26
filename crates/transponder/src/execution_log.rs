//! Frame-stream assembly and the chamber execution record.
//!
//! Two pieces the `Source::Airlock` frame-consumption path uses:
//!
//!  - [`assemble_from_frames`] folds a completed typed-frame stream into the
//!    model-facing tool result AND the raw execution record. stdout + image
//!    frames form the model result; stderr is excluded from the model result
//!    EXCEPT on a non-zero exit (a survived failure), when it is folded into the
//!    result so the failure detail reaches the caller; stdout + stderr both feed
//!    the execution record.
//!  - [`LocalFsExecutionLog`] is the transponder-authored, chamber-unwritable
//!    execution-log store: one file per `call_id`, holding the tool's stdout and
//!    stderr — the same trust posture as the conversation log.
//!
use std::path::PathBuf;

use airlock_proto::tool_result_frame::Frame;
use airlock_proto::ToolResultFrame;
use proto_common::CallToolResponse;

/// The raw per-call execution record persisted to the execution log: the tool's
/// scrubbed stdout and stderr plus its exit code. Distinct from the
/// conversation log — it holds the "how", keyed by `call_id`.
pub(crate) struct ExecutionRecord {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Fold a completed frame stream into `(model-facing result, execution record)`.
/// stdout and image frames form the model-facing content; stderr is excluded
/// from it EXCEPT on a non-zero exit (a survived failure), when it is appended
/// so the failure detail reaches the caller. stdout and stderr both feed the
/// execution record. The terminal `ToolComplete` carries `is_error`/`exit_code`.
pub(crate) fn assemble_from_frames(
    frames: &[ToolResultFrame],
) -> (CallToolResponse, ExecutionRecord) {
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut images: Vec<proto_common::ContentBlock> = Vec::new();
    let mut exit_code = 0;
    let mut is_error = false;

    for frame in frames {
        match frame.frame.as_ref() {
            Some(Frame::Stdout(s)) => push_line(&mut stdout, s),
            Some(Frame::Stderr(s)) => push_line(&mut stderr, s),
            Some(Frame::Image(img)) => images.push(proto_common::image_block(
                img.media_type.clone(),
                img.data.clone(),
            )),
            Some(Frame::Complete(c)) => {
                exit_code = c.exit_code;
                is_error = c.is_error;
            }
            None => {}
        }
    }

    // Model-facing content: image frames, then a stdout text block. On a
    // survived non-zero exit the stderr is folded into that text so the failure
    // detail reaches the caller (routed to the model or the client by the call
    // site). A pure-image success carries no empty text block.
    let mut text = stdout.clone();
    if exit_code != 0 && !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    let mut content = images;
    if content.is_empty() || !text.is_empty() {
        content.push(proto_common::text_block(text));
    }

    (
        CallToolResponse { content, is_error },
        ExecutionRecord {
            stdout,
            stderr,
            exit_code,
        },
    )
}

/// Append a frame's text as a line, joining successive frames with `\n` (the
/// per-line boundary the chamber split on before scrubbing).
fn push_line(buf: &mut String, line: &str) {
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(line);
}

/// Write-only execution-log store. The transponder is the sole author; the
/// chamber's separate pod cannot write here.
#[async_trait::async_trait]
pub(crate) trait ExecutionLogWriter: Send + Sync {
    async fn write(&self, call_id: &str, record: &ExecutionRecord) -> Result<(), String>;
}

/// Local-filesystem execution log: one JSON file per `call_id` under a
/// transponder-owned directory on its existing PVC. The chamber's separate pod
/// cannot write here, the same trust posture as the conversation log.
pub(crate) struct LocalFsExecutionLog {
    root: PathBuf,
}

impl LocalFsExecutionLog {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait::async_trait]
impl ExecutionLogWriter for LocalFsExecutionLog {
    async fn write(&self, call_id: &str, record: &ExecutionRecord) -> Result<(), String> {
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| format!("create execution-log dir: {e}"))?;
        let body = serde_json::json!({
            "call_id": call_id,
            "stdout": record.stdout,
            "stderr": record.stderr,
            "exit_code": record.exit_code,
        })
        .to_string();
        // One file per call_id. `call_id` is a controller-minted uuid, so it is
        // a safe filename; guard against a path separator regardless.
        let safe = call_id.replace(['/', '\\'], "_");
        let path = self.root.join(format!("exec-{safe}.json"));
        tokio::fs::write(&path, body)
            .await
            .map_err(|e| format!("write execution log {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airlock_proto::{tool_result_frame::Frame, ToolComplete};

    fn stdout_f(s: &str) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Stdout(s.into())),
        }
    }
    fn stderr_f(s: &str) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Stderr(s.into())),
        }
    }
    fn image_f(media_type: &str, data: Vec<u8>) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Image(proto_common::ImageBlock {
                media_type: media_type.into(),
                data,
            })),
        }
    }
    fn complete_f(is_error: bool, exit_code: i32) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Complete(ToolComplete {
                is_error,
                exit_code,
            })),
        }
    }

    fn has_image(resp: &CallToolResponse) -> bool {
        resp.content.iter().any(|b| {
            matches!(
                b.block.as_ref(),
                Some(proto_common::content_block::Block::Image(_))
            )
        })
    }

    // The model-facing result assembled from a frame stream includes stdout and
    // image frames and excludes stderr frames on a successful run.
    //
    // Materiality: the stub returns empty content, reding the stdout/image
    // assertions. A mutant that folds stderr into the model result on success
    // (today's behavior) reds the "stderr excluded" assertion; dropping the
    // image frame from the result reds the image assertion.
    #[test]
    fn model_result_includes_stdout_and_image_and_excludes_stderr_on_success() {
        let frames = vec![
            stdout_f("visible stdout line"),
            stderr_f("hidden stderr detail"),
            image_f("image/png", vec![1, 2, 3]),
            complete_f(false, 0),
        ];
        let (result, _record) = assemble_from_frames(&frames);
        let text = proto_common::content_text(&result.content);
        assert!(
            text.contains("visible stdout line"),
            "stdout belongs in the model-facing result, got {text:?}"
        );
        assert!(
            !text.contains("hidden stderr detail"),
            "stderr is excluded from the model-facing result on a successful run"
        );
        assert!(
            has_image(&result),
            "an image frame belongs in the model result"
        );
        assert!(!result.is_error, "a zero-exit result is not an error");
    }

    // On a survived non-zero exit the tool's stderr IS folded into the failure
    // result. The fold is issuer-agnostic (no issuer parameter): the agent call
    // site routes this content into the model message and the user call site
    // returns it in the RPC response to the client — pre-existing plumbing.
    //
    // Materiality: the stub returns empty content, reding the stderr-present
    // assertion. A mutant that never folds stderr (always excludes it, even on
    // failure) reds the "boom" assertion; a mutant that folds stderr on ALL
    // exits reds the success-case test above — together they pin the
    // exit-conditional fold.
    #[test]
    fn non_zero_exit_folds_stderr_into_the_failure_result() {
        let frames = vec![
            stdout_f("partial output before the failure"),
            stderr_f("boom: the failure reason"),
            complete_f(true, 2),
        ];
        let (result, record) = assemble_from_frames(&frames);
        let text = proto_common::content_text(&result.content);
        assert!(
            text.contains("boom: the failure reason"),
            "on a non-zero exit the tool's stderr enters the result, got {text:?}"
        );
        assert!(result.is_error, "a non-zero exit produces an error result");
        assert_eq!(
            record.exit_code, 2,
            "the execution record carries the child's non-zero exit code"
        );
    }

    // Fold join format (kills the surviving mutant at the `if !text.is_empty()`
    // guard on the stdout/stderr fold): on a non-zero exit with BOTH non-empty
    // stdout and non-empty stderr, the folded model text is exactly
    // `stdout` + one `\n` + `stderr` — one separator, stderr trailing.
    //
    // Materiality: the stderr-fold test above only asserts the stderr substring
    // is present, so `stdoutstderr` (no separator) still passes it — the mutant
    // survives. Deleting the `!` in `if !text.is_empty()` (→ `if text.is_empty()`)
    // skips the `\n` when stdout is non-empty, yielding
    // `"out lineerr line"`; dropping the `text.push('\n')` line does the same.
    // Either reds the exact-join assertion below.
    #[test]
    fn non_zero_exit_joins_stdout_and_stderr_with_exactly_one_newline() {
        let frames = vec![
            stdout_f("out line"),
            stderr_f("err line"),
            complete_f(true, 3),
        ];
        let (result, _record) = assemble_from_frames(&frames);
        let text = proto_common::content_text(&result.content);
        assert_eq!(
            text, "out line\nerr line",
            "stdout and the folded stderr join as stdout-newline-stderr, got {text:?}"
        );
    }

    // The execution record captures BOTH the tool's stdout and its stderr — the
    // observability "how" the model result deliberately drops.
    //
    // Materiality: the stub returns empty strings, reding both assertions. A
    // mutant that records only stdout (dropping stderr) reds the stderr
    // assertion — the exact information the execution log exists to keep.
    #[test]
    fn execution_record_captures_both_stdout_and_stderr() {
        let frames = vec![
            stdout_f("recorded stdout A"),
            stderr_f("recorded stderr B"),
            complete_f(false, 0),
        ];
        let (_result, record) = assemble_from_frames(&frames);
        assert!(
            record.stdout.contains("recorded stdout A"),
            "execution record keeps stdout, got {:?}",
            record.stdout
        );
        assert!(
            record.stderr.contains("recorded stderr B"),
            "execution record keeps stderr, got {:?}",
            record.stderr
        );
    }

    // The transponder's LocalFsExecutionLog persists an execution record keyed
    // by its call_id, holding stdout AND stderr, under its own transponder-owned
    // root. This is the write half of "only the transponder writes the execution
    // log"; the chamber-unforgeable half
    // (separate PVC + RBAC, same posture as the conversation log) is an
    // infrastructure property, not unit-testable.
    //
    // Materiality: the stub `write` is a no-op, so no file is persisted, reding
    // `found.expect`. A mutant that persists without keying by call_id, or that
    // drops stdout or stderr from the persisted body, reds the corresponding
    // content assertion.
    #[tokio::test]
    async fn execution_log_persists_a_record_keyed_by_call_id_holding_stdout_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf());
        let record = ExecutionRecord {
            stdout: "persisted stdout".into(),
            stderr: "persisted stderr".into(),
            exit_code: 0,
        };
        log.write("call-777", &record)
            .await
            .expect("the transponder must persist the execution record");

        let mut found: Option<String> = None;
        for de in std::fs::read_dir(dir.path()).unwrap().flatten() {
            let name = de.file_name().to_string_lossy().into_owned();
            let body = std::fs::read_to_string(de.path()).unwrap_or_default();
            if name.contains("call-777") || body.contains("call-777") {
                found = Some(body);
            }
        }
        let body = found.expect("an execution-log file keyed by the call_id must be persisted");
        assert!(
            body.contains("persisted stdout"),
            "the persisted execution record holds stdout, got {body:?}"
        );
        assert!(
            body.contains("persisted stderr"),
            "the persisted execution record holds stderr, got {body:?}"
        );
    }
}
