//! Frame-stream assembly and the toolset execution-log store.
//!
//!  - [`assemble_from_frames`] folds a completed typed-frame stream into the
//!    model-facing tool result. The `Source::Toolset` agent-turn arm assembles
//!    its return value here.
//!  - [`ExecutionLogWriter`] / [`LocalFsExecutionLog`] is the harness-
//!    authored, toolset-unwritable execution-log store, with the same trust
//!    posture as the conversation log. A store is the agent's one append-only
//!    `execution.json` at the conversation root (ND-JSON, one
//!    record per frame, each carrying its `call_id`). `append_frame` records
//!    stdout/stderr frames and the terminal record as they arrive; a
//!    produced-artifact image frame is not persisted. `read` replays a single
//!    call's ordered stream as a [`PersistedCall`] by filtering `execution.json`
//!    to that `call_id`. Both the agent-turn arm and the client-driven
//!    dispatch/await path write through this store, so a dropped or
//!    re-subscribed call is served without re-running the tool.
//!
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::conversation::parse_ndjson;
use proto_common::tool_result_frame::Frame;
use proto_common::{CallToolResponse, ToolOutcome, ToolResultFrame};

/// Fold a completed frame stream into the model-facing tool result. stdout and
/// image frames form the model-facing content; stderr is excluded from it EXCEPT
/// on a non-zero exit (a survived failure), when it is appended so the failure
/// detail reaches the caller. The terminal `ToolComplete` carries the outcome
/// (which derives `is_error`) and the exit code. The observability "how" —
/// stdout and stderr keyed by `call_id` — is persisted separately by
/// [`ExecutionLogWriter::append_frame`] as each frame arrives.
pub(crate) fn assemble_from_frames(frames: &[ToolResultFrame]) -> CallToolResponse {
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
                // Derived error is a pure function of the outcome: every
                // non-DONE terminal (FAILED or CANCELED) is an error at the
                // assembled-response level. Error and outcome cannot contradict.
                is_error = c.outcome() != ToolOutcome::Done;
            }
            None => {}
        }
    }

    // Model-facing content: image frames, then a stdout text block. On a
    // survived non-zero exit the stderr is folded into that text so the failure
    // detail reaches the caller. A pure-image success carries no empty text
    // block.
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

    CallToolResponse { content, is_error }
}

/// Split an in-process tool result into the frame stream the client-facing
/// dispatch/await path carries. The inverse of [`assemble_from_frames`]: each
/// text block becomes a `Stdout` frame, each image block an `Image` frame, and
/// the stream closes on a terminal whose outcome derives from `is_error`.
///
/// A `Runtime`-source tool produces its result in-process, with no frame
/// stream of its own. This is what gives it one, so the
/// client's await, the execution log, and the client-side fold all see the same
/// shape they see for a toolset call. `Text` and `Image` are the only content
/// blocks with a frame representation; anything else is dropped with a warning
/// rather than silently.
pub(crate) fn frames_from_response(response: &CallToolResponse) -> Vec<ToolResultFrame> {
    let mut frames = Vec::with_capacity(response.content.len() + 1);
    for block in &response.content {
        let frame = match block.block.as_ref() {
            Some(proto_common::content_block::Block::Text(t)) => Frame::Stdout(t.text.clone()),
            Some(proto_common::content_block::Block::Image(img)) => Frame::Image(img.clone()),
            other => {
                tracing::warn!(
                    block = ?other,
                    "dropping a content block with no tool-result frame representation"
                );
                continue;
            }
        };
        frames.push(ToolResultFrame { frame: Some(frame) });
    }
    // The outcome is the single source of the client's error bit, so it must
    // derive from `is_error` and never be fixed. The exit code follows it so a
    // consumer keying on either agrees with the other.
    let (outcome, exit_code) = if response.is_error {
        (ToolOutcome::Failed, 1)
    } else {
        (ToolOutcome::Done, 0)
    };
    frames.push(ToolResultFrame {
        frame: Some(Frame::Complete(proto_common::ToolComplete {
            outcome: outcome as i32,
            exit_code,
        })),
    });
    frames
}

/// Append a frame's text as a line, joining successive frames with `\n` (the
/// per-line boundary the toolset split on before scrubbing).
fn push_line(buf: &mut String, line: &str) {
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(line);
}

/// A call's persisted frame record, replayed to a re-subscriber. Holds the
/// whole ordered stream — every pre-terminal frame plus the terminal — so a
/// dropped result stream is served from the store without re-running the tool.
pub(crate) struct PersistedCall {
    frames: Vec<ToolResultFrame>,
}

impl PersistedCall {
    /// The persisted frames in arrival order.
    pub(crate) fn frames(&self) -> &[ToolResultFrame] {
        &self.frames
    }

    /// Whether the record's last frame is the terminal `ToolComplete` — the
    /// call finished and the record is complete.
    pub(crate) fn has_terminal(&self) -> bool {
        matches!(
            self.frames.last().and_then(|f| f.frame.as_ref()),
            Some(Frame::Complete(_))
        )
    }
}

/// Execution-log store. The harness is the sole author; the toolset's
/// separate pod cannot write here.
#[async_trait::async_trait]
pub(crate) trait ExecutionLogWriter: Send + Sync {
    /// Append one typed frame to the call's record as it arrives. The terminal
    /// frame is the last appended and marks the record finished.
    async fn append_frame(&self, call_id: &str, frame: &ToolResultFrame) -> Result<(), String>;

    /// Read a call's persisted frame record for replay, or `None` when no
    /// record exists for the id.
    async fn read(&self, call_id: &str) -> Option<PersistedCall>;
}

/// The agent's one `execution.json` at the conversation root, append-only
/// ND-JSON; each line is one [`ExecutionRecord`].
pub(crate) const EXECUTION_LOG_FILENAME: &str = "execution.json";

/// One persisted execution-log line: a typed frame tagged with the `call_id`
/// it belongs to, so a single `execution.json` holds every call and a
/// re-subscriber filters by id. Harness-local (the shared `proto_common` types
/// carry no serde); converted to/from [`ToolResultFrame`] on the store
/// boundary.
#[derive(Serialize, Deserialize)]
struct ExecutionRecord {
    call_id: String,
    #[serde(flatten)]
    body: RecordBody,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
enum RecordBody {
    Stdout { text: String },
    Stderr { text: String },
    Complete { outcome: i32, exit_code: i32 },
}

/// Not rotated with the conversation log.
pub(crate) struct LocalFsExecutionLog {
    root: PathBuf,
    /// Serializes concurrent appends to the agent's `execution.json`
    /// (the model-turn path and the app-run path) so their multi-syscall
    /// writes never interleave a line.
    append_lock: tokio::sync::Mutex<()>,
}

impl LocalFsExecutionLog {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            append_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn execution_log_path(&self) -> PathBuf {
        self.root.join(EXECUTION_LOG_FILENAME)
    }
}

/// Build the persisted record for a frame. `None` for an empty frame (nothing
/// to persist) and for a produced-artifact image frame (deliberately dropped).
fn frame_to_record(call_id: &str, frame: &ToolResultFrame) -> Option<ExecutionRecord> {
    let body = match frame.frame.as_ref()? {
        Frame::Stdout(s) => RecordBody::Stdout { text: s.clone() },
        Frame::Stderr(s) => RecordBody::Stderr { text: s.clone() },
        Frame::Image(_) => return None,
        Frame::Complete(c) => RecordBody::Complete {
            outcome: c.outcome,
            exit_code: c.exit_code,
        },
    };
    Some(ExecutionRecord {
        call_id: call_id.to_string(),
        body,
    })
}

#[async_trait::async_trait]
impl ExecutionLogWriter for LocalFsExecutionLog {
    async fn append_frame(&self, call_id: &str, frame: &ToolResultFrame) -> Result<(), String> {
        let Some(record) = frame_to_record(call_id, frame) else {
            return Ok(());
        };
        let mut line = serde_json::to_string(&record)
            .map_err(|e| format!("serialize execution record: {e}"))?;
        line.push('\n');

        // Serialize the whole append so two calls' multi-syscall writes to the
        // shared file never splice a line.
        let _guard = self.append_lock.lock().await;

        // The append runs in a blocking closure over `std::fs` so the file is
        // opened, written, and closed (flushed) before this returns — a plain
        // `tokio::fs::File` does not flush its buffer on drop, which would leave
        // the line unpersisted when a caller reads back immediately.
        let path = self.execution_log_path();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| format!("open execution log {}: {e}", path.display()))?;
            file.write_all(line.as_bytes())
                .map_err(|e| format!("append execution record {}: {e}", path.display()))
        })
        .await
        .map_err(|e| format!("append_frame join error: {e}"))?
    }

    async fn read(&self, call_id: &str) -> Option<PersistedCall> {
        let text = tokio::fs::read_to_string(self.execution_log_path())
            .await
            .ok()?;
        // A torn last line is cut at boot; any other unparsable line is
        // skipped.
        let frames: Vec<ToolResultFrame> = parse_ndjson::<ExecutionRecord>(&text, "execution log")
            .into_iter()
            .filter(|record| record.call_id == call_id)
            .map(|record| match record.body {
                RecordBody::Stdout { text } => ToolResultFrame {
                    frame: Some(Frame::Stdout(text)),
                },
                RecordBody::Stderr { text } => ToolResultFrame {
                    frame: Some(Frame::Stderr(text)),
                },
                RecordBody::Complete { outcome, exit_code } => ToolResultFrame {
                    frame: Some(Frame::Complete(proto_common::ToolComplete {
                        outcome,
                        exit_code,
                    })),
                },
            })
            .collect();
        if frames.is_empty() {
            None
        } else {
            Some(PersistedCall { frames })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_doubles::{done_terminal, stdout_frame};
    use crate::tool_router::is_terminal_frame;
    use proto_common::ToolComplete;
    use std::path::Path;
    use std::sync::Arc;
    fn stderr_frame(s: &str) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Stderr(s.into())),
        }
    }
    fn image_frame(media_type: &str, data: Vec<u8>) -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Image(proto_common::ImageBlock {
                media_type: media_type.into(),
                data,
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

    fn stdout_text(frame: &ToolResultFrame) -> Option<&str> {
        match frame.frame.as_ref() {
            Some(Frame::Stdout(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Non-empty newline-delimited records of a log file (empty vec if absent).
    fn ndjson_records(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect()
    }

    // The model-facing result assembled from a frame stream includes stdout and
    // image frames and excludes stderr frames on a successful run.
    #[test]
    fn model_result_includes_stdout_and_image_and_excludes_stderr_on_success() {
        let frames = vec![
            stdout_frame("visible stdout line"),
            stderr_frame("hidden stderr detail"),
            image_frame("image/png", vec![1, 2, 3]),
            done_terminal(),
        ];
        let result = assemble_from_frames(&frames);
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
    // result, joined to stdout by exactly one newline.
    #[tokio::test]
    async fn non_zero_exit_folds_stderr_into_the_failure_result() {
        let frames = vec![
            stdout_frame("partial output before the failure"),
            stderr_frame("boom: the failure reason"),
            ToolResultFrame {
                frame: Some(Frame::Complete(ToolComplete {
                    outcome: ToolOutcome::Failed as i32,
                    exit_code: 2,
                })),
            },
        ];
        let result = assemble_from_frames(&frames);
        let text = proto_common::content_text(&result.content);
        assert_eq!(
            text, "partial output before the failure\nboom: the failure reason",
            "on a non-zero exit stderr enters the result as stdout-newline-stderr, got {text:?}"
        );
        assert!(result.is_error, "a non-zero exit produces an error result");
    }

    // Two tool calls writing concurrently to the one execution.json
    // serialize their appends so no line interleaves.
    //
    // Large payloads force multi-syscall writes so the unguarded path corrupts
    // observably.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_do_not_interleave() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(LocalFsExecutionLog::new(dir.path().to_path_buf()));

        const PER_CALL: usize = 30;
        let payload_a = "A".repeat(16 * 1024);
        let payload_b = "B".repeat(16 * 1024);

        let task_a = {
            let log = log.clone();
            tokio::spawn(async move {
                for _ in 0..PER_CALL {
                    log.append_frame("call-A", &stdout_frame(&payload_a))
                        .await
                        .expect("append A");
                }
            })
        };
        let task_b = {
            let log = log.clone();
            tokio::spawn(async move {
                for _ in 0..PER_CALL {
                    log.append_frame("call-B", &stdout_frame(&payload_b))
                        .await
                        .expect("append B");
                }
            })
        };
        task_a.await.unwrap();
        task_b.await.unwrap();

        let exec_json = dir.path().join("execution.json");
        assert!(
            exec_json.is_file(),
            "both calls share one execution.json, none at {}",
            exec_json.display()
        );
        let records = ndjson_records(&exec_json);
        assert_eq!(
            records.len(),
            PER_CALL * 2,
            "every appended frame is exactly one line, none lost or spliced, got {}",
            records.len()
        );
        for line in &records {
            serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| {
                panic!("a serialized append must not interleave into a corrupt line: {e}")
            });
        }

        let a = log.read("call-A").await.expect("call-A record");
        let b = log.read("call-B").await.expect("call-B record");
        assert_eq!(
            a.frames().len(),
            PER_CALL,
            "call-A replays exactly its own frames"
        );
        assert_eq!(
            b.frames().len(),
            PER_CALL,
            "call-B replays exactly its own frames"
        );
    }

    // An unparsable line is skipped; the records around it still replay.
    //
    // The good records are written through the real API so the test does not
    // hard-code the record schema.
    #[tokio::test]
    async fn reader_skips_an_unparsable_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf());

        log.append_frame("call-tol", &stdout_frame("good-line-before"))
            .await
            .expect("append stdout");

        let exec_json = dir.path().join("execution.json");
        assert!(
            exec_json.is_file(),
            "records must be persisted to execution.json before the tolerance check, none at {}",
            exec_json.display()
        );
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&exec_json)
                .unwrap();
            f.write_all(b"{\"call_id\":\"call-tol\",\"frame\":\"stdo\n")
                .unwrap();
        }

        log.append_frame("call-tol", &done_terminal())
            .await
            .expect("append terminal");

        let persisted = log
            .read("call-tol")
            .await
            .expect("an unparsable line must not make the read fail");
        assert!(
            persisted
                .frames()
                .iter()
                .any(|f| stdout_text(f) == Some("good-line-before")),
            "the record before the unparsable line still replays"
        );
        assert!(
            persisted.frames().iter().any(is_terminal_frame),
            "the terminal after the unparsable line still replays"
        );
    }

    // A re-subscribe to a call by id replays only that call's frames, filtered by
    // call_id from a shared execution.json holding several calls.
    #[tokio::test]
    async fn resubscribe_replays_a_call_filtered_from_a_shared_execution_json() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf());

        log.append_frame("call-A", &stdout_frame("AAA-marker"))
            .await
            .unwrap();
        log.append_frame("call-B", &stdout_frame("BBB-marker"))
            .await
            .unwrap();
        log.append_frame("call-A", &done_terminal()).await.unwrap();
        log.append_frame("call-B", &done_terminal()).await.unwrap();

        let exec_json = dir.path().join("execution.json");
        let text = std::fs::read_to_string(&exec_json).expect("single shared execution.json");
        assert!(
            text.contains("AAA-marker") && text.contains("BBB-marker"),
            "both calls' frames share one execution.json, got {text:?}"
        );

        let a = log.read("call-A").await.expect("call-A replay");
        assert_eq!(
            a.frames().len(),
            2,
            "call-A replays only its own two frames, got {}",
            a.frames().len()
        );
        assert!(
            a.frames()
                .iter()
                .any(|f| stdout_text(f) == Some("AAA-marker")),
            "call-A's own stdout is replayed"
        );
        assert!(
            !a.frames()
                .iter()
                .any(|f| stdout_text(f) == Some("BBB-marker")),
            "call-A's replay is filtered: it must not carry call-B's frames"
        );

        let b = log.read("call-B").await.expect("call-B replay");
        assert_eq!(
            b.frames().len(),
            2,
            "call-B replays only its own two frames"
        );
        assert!(
            !b.frames()
                .iter()
                .any(|f| stdout_text(f) == Some("AAA-marker")),
            "call-B's replay is filtered: it must not carry call-A's frames"
        );
    }

    // A produced image writes nothing beside execution.json and logs no image
    // record. Only stdout/stderr/terminal are the execution log's
    // business, and dropping the image must not disturb the stdout/stderr
    // frames streamed around it.
    #[tokio::test]
    async fn a_produced_image_is_dropped_from_the_execution_log_but_its_neighbors_kept() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf());

        for frame in [
            stdout_frame("stdout before the image"),
            image_frame("image/png", vec![1, 2, 3]),
            stderr_frame("stderr after the image"),
            done_terminal(),
        ] {
            log.append_frame("call-img", &frame)
                .await
                .expect("append frame");
        }

        // (a) Nothing is written beside execution.json for the produced image.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            names,
            ["execution.json"],
            "a produced-artifact image writes nothing beside execution.json; found {names:?}"
        );

        // (b) No image record line is persisted.
        let exec_json = dir.path().join("execution.json");
        let records = ndjson_records(&exec_json);
        assert!(
            !records.iter().any(|l| l.contains("\"frame\":\"image\"")),
            "no image record line is persisted for a produced artifact, got {records:?}"
        );

        // (c) The stdout/stderr frames around the dropped image still stream.
        let persisted = log.read("call-img").await.expect("persisted record");
        assert!(
            persisted.frames().iter().any(
                |f| matches!(f.frame.as_ref(), Some(Frame::Stdout(s)) if s == "stdout before the image")
            ),
            "the stdout frame before the image is still recorded"
        );
        assert!(
            persisted.frames().iter().any(
                |f| matches!(f.frame.as_ref(), Some(Frame::Stderr(s)) if s == "stderr after the image")
            ),
            "the stderr frame after the image is still recorded"
        );
    }

    // Frames appended one at a time are read back in arrival order, every
    // pre-terminal frame included, with the terminal flagged present.
    #[tokio::test]
    async fn read_returns_appended_frames_in_order_with_terminal_present() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf());

        log.append_frame("call-replay", &stdout_frame("first stdout"))
            .await
            .expect("append stdout frame");
        log.append_frame("call-replay", &stderr_frame("second stderr"))
            .await
            .expect("append stderr frame");
        let canceled = ToolResultFrame {
            frame: Some(Frame::Complete(ToolComplete {
                outcome: ToolOutcome::Canceled as i32,
                exit_code: -1,
            })),
        };
        log.append_frame("call-replay", &canceled)
            .await
            .expect("append terminal frame");

        let persisted = log
            .read("call-replay")
            .await
            .expect("a re-subscriber must be served the persisted record");
        let frames = persisted.frames();
        assert_eq!(
            frames.len(),
            3,
            "every appended frame is replayed, not just the terminal, got {}",
            frames.len()
        );
        assert!(
            matches!(frames[0].frame.as_ref(), Some(Frame::Stdout(s)) if s == "first stdout"),
            "the first appended frame replays first"
        );
        assert!(
            matches!(frames[1].frame.as_ref(), Some(Frame::Stderr(s)) if s == "second stderr"),
            "the second appended frame replays second"
        );
        assert!(
            is_terminal_frame(&frames[2]),
            "the terminal frame replays last"
        );
        assert!(
            persisted.has_terminal(),
            "a record whose last frame is a ToolComplete reports the terminal present"
        );
    }
}
