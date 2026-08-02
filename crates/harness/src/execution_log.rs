//! Frame-stream assembly and the chamber execution-log store.
//!
//!  - [`assemble_from_frames`] folds a completed typed-frame stream into the
//!    model-facing tool result. The `Source::Airlock` agent-turn arm assembles
//!    its return value here.
//!  - [`ExecutionLogWriter`] / [`LocalFsExecutionLog`] is the harness-
//!    authored, chamber-unwritable execution-log store, with the same trust
//!    posture as the conversation log. A store is scoped to one conversation
//!    directory: it holds a single append-only `execution.json` (ND-JSON, one
//!    record per frame, each carrying its `call_id`). `append_frame` records
//!    stdout/stderr frames and the terminal record as they arrive; a
//!    produced-artifact image frame is not persisted. `read` replays a single
//!    call's ordered stream as a [`PersistedCall`] by filtering `execution.json`
//!    to that `call_id`; the `blobs/sha256/<hex>` tree is read-only, retained
//!    only to replay previously written image records. Both
//!    the agent-turn arm and the client-driven dispatch/await path write
//!    through this store, so a dropped or re-subscribed call is served without
//!    re-running the tool.
//!
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
#[cfg(test)]
use sha2::{Digest, Sha256};

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

    CallToolResponse { content, is_error }
}

/// Append a frame's text as a line, joining successive frames with `\n` (the
/// per-line boundary the chamber split on before scrubbing).
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

/// Execution-log store. The harness is the sole author; the chamber's
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

/// The execution-log filename under a conversation directory. One append-only
/// ND-JSON file per conversation; each line is one [`ExecutionRecord`].
const EXECUTION_LOG_FILENAME: &str = "execution.json";

/// One persisted execution-log line: a typed frame tagged with the `call_id`
/// it belongs to, so a single `execution.json` holds every call for the
/// conversation and a re-subscriber filters by id. Harness-local (the
/// shared `proto_common` types carry no serde); converted to/from
/// [`ToolResultFrame`] on the store boundary. An image record carries the
/// frame's `media_type` and the sha256 digest of its bytes — never the bytes
/// inline; those live in the content-addressed blob tree.
#[derive(Serialize, Deserialize)]
struct ExecutionRecord {
    call_id: String,
    /// The owning conversation's id, stamped so the on-disk record is
    /// self-describing: resolution reads the owning conversation off the
    /// record itself. `#[serde(default)]` so records written before the field
    /// existed still parse (empty string), rather than failing the whole line.
    #[serde(default)]
    conversation_id: String,
    #[serde(flatten)]
    body: RecordBody,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
enum RecordBody {
    Stdout { text: String },
    Stderr { text: String },
    Image { media_type: String, sha256: String },
    Complete { outcome: i32, exit_code: i32 },
}

/// Hex SHA-256 of raw bytes — the content address of a binary frame's blob.
#[cfg(test)]
fn sha256_hex_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Local-filesystem execution log scoped to one conversation directory. Holds
/// a single append-only `execution.json` (ND-JSON, one line per frame, each
/// tagged with its `call_id`) plus a `blobs/sha256/<hex>` tree for binary
/// frames. The chamber's separate pod cannot write here, the same trust
/// posture as the conversation log.
pub(crate) struct LocalFsExecutionLog {
    /// The conversation directory. `execution.json` and `blobs/` live directly
    /// under it, siblings of the conversation log's `conversation.json`.
    root: PathBuf,
    /// The owning conversation's id. The writer is conversation-scoped (one per
    /// conv_id), so its conversation id is a construction-time invariant, not a
    /// per-call argument. Stamped into every appended record.
    conversation_id: String,
    /// Serializes concurrent appends to this conversation's `execution.json`
    /// (the model-turn path and the app-run path) so their multi-syscall
    /// writes never interleave a line.
    append_lock: tokio::sync::Mutex<()>,
}

impl LocalFsExecutionLog {
    pub(crate) fn new(root: PathBuf, conversation_id: String) -> Self {
        Self {
            root,
            conversation_id,
            append_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn execution_log_path(&self) -> PathBuf {
        self.root.join(EXECUTION_LOG_FILENAME)
    }

    fn blob_dir(&self) -> PathBuf {
        self.root.join("blobs").join("sha256")
    }

    /// Load a blob's bytes back for replay.
    async fn read_blob(dir: &Path, hex: &str) -> Option<Vec<u8>> {
        tokio::fs::read(dir.join(hex)).await.ok()
    }
}

/// Build the persisted record for a frame. `None` for an empty frame (nothing
/// to persist) and for a produced-artifact image frame (deliberately dropped).
fn frame_to_record(
    call_id: &str,
    conversation_id: &str,
    frame: &ToolResultFrame,
) -> Option<ExecutionRecord> {
    let body = match frame.frame.as_ref()? {
        Frame::Stdout(s) => RecordBody::Stdout { text: s.clone() },
        Frame::Stderr(s) => RecordBody::Stderr { text: s.clone() },
        // Produced-artifact bytes are not recorded in the execution log; only
        // stdout/stderr and the terminal are.
        Frame::Image(_) => return None,
        Frame::Complete(c) => RecordBody::Complete {
            outcome: c.outcome,
            exit_code: c.exit_code,
        },
    };
    Some(ExecutionRecord {
        call_id: call_id.to_string(),
        conversation_id: conversation_id.to_string(),
        body,
    })
}

#[async_trait::async_trait]
impl ExecutionLogWriter for LocalFsExecutionLog {
    async fn append_frame(&self, call_id: &str, frame: &ToolResultFrame) -> Result<(), String> {
        let Some(record) = frame_to_record(call_id, &self.conversation_id, frame) else {
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
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            use std::io::Write;
            std::fs::create_dir_all(&root).map_err(|e| format!("create execution-log dir: {e}"))?;
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
        let blob_dir = self.blob_dir();
        let mut frames = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // Tolerant reader: a half-written trailing line (torn on a crash
            // mid-append) fails to parse and is skipped, not fatal.
            let record: ExecutionRecord = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "skipping unparsable execution log line");
                    continue;
                }
            };
            if record.call_id != call_id {
                continue;
            }
            let frame = match record.body {
                RecordBody::Stdout { text } => ToolResultFrame {
                    frame: Some(Frame::Stdout(text)),
                },
                RecordBody::Stderr { text } => ToolResultFrame {
                    frame: Some(Frame::Stderr(text)),
                },
                RecordBody::Image { media_type, sha256 } => {
                    let Some(data) = Self::read_blob(&blob_dir, &sha256).await else {
                        tracing::warn!(sha256, "missing blob for image record; skipping frame");
                        continue;
                    };
                    ToolResultFrame {
                        frame: Some(Frame::Image(proto_common::ImageBlock { media_type, data })),
                    }
                }
                RecordBody::Complete { outcome, exit_code } => ToolResultFrame {
                    frame: Some(Frame::Complete(proto_common::ToolComplete {
                        outcome,
                        exit_code,
                    })),
                },
            };
            frames.push(frame);
        }
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
    use proto_common::{tool_result_frame::Frame, ToolComplete, ToolOutcome};
    use std::path::Path;
    use std::sync::Arc;

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
        let outcome = if is_error {
            ToolOutcome::Failed
        } else {
            ToolOutcome::Done
        };
        ToolResultFrame {
            frame: Some(Frame::Complete(ToolComplete {
                outcome: outcome as i32,
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

    fn stdout_text(frame: &ToolResultFrame) -> Option<&str> {
        match frame.frame.as_ref() {
            Some(Frame::Stdout(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    fn is_terminal(frame: &ToolResultFrame) -> bool {
        matches!(frame.frame.as_ref(), Some(Frame::Complete(_)))
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
    // result, and the child's exit code survives in the persisted `.frames`
    // record's terminal frame. The fold is issuer-agnostic (no issuer
    // parameter): the agent call site routes this content into the model message
    // and the user call site returns it in the RPC response to the client.
    //
    // Materiality: a mutant that never folds stderr (always excludes it, even on
    // failure) reds the "boom" assertion; a mutant that folds stderr on ALL exits
    // reds the success-case test above — together they pin the exit-conditional
    // fold. A mutant that drops the terminal's exit code before persisting reds
    // the exit-code assertion read back from the `.frames` store.
    #[tokio::test]
    async fn non_zero_exit_folds_stderr_into_the_failure_result() {
        let frames = vec![
            stdout_f("partial output before the failure"),
            stderr_f("boom: the failure reason"),
            complete_f(true, 2),
        ];
        let result = assemble_from_frames(&frames);
        let text = proto_common::content_text(&result.content);
        assert!(
            text.contains("boom: the failure reason"),
            "on a non-zero exit the tool's stderr enters the result, got {text:?}"
        );
        assert!(result.is_error, "a non-zero exit produces an error result");

        // The exit code survives in the terminal frame persisted to the `.frames`
        // store. Append the stream and read it back to assert the child's non-zero
        // exit reached the record.
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf(), "test-conv".to_string());
        for frame in &frames {
            log.append_frame("call-exit", frame)
                .await
                .expect("append frame");
        }
        let persisted = log.read("call-exit").await.expect("persisted record");
        let exit_code = persisted
            .frames()
            .iter()
            .find_map(|f| match f.frame.as_ref() {
                Some(Frame::Complete(c)) => Some(c.exit_code),
                _ => None,
            });
        assert_eq!(
            exit_code,
            Some(2),
            "the persisted record's terminal frame carries the child's non-zero exit code"
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
        let result = assemble_from_frames(&frames);
        let text = proto_common::content_text(&result.content);
        assert_eq!(
            text, "out line\nerr line",
            "stdout and the folded stderr join as stdout-newline-stderr, got {text:?}"
        );
    }

    // The persisted execution record captures BOTH the tool's stdout and its
    // stderr, keyed by call_id — the observability "how" the model result
    // deliberately drops. With one on-disk format they live as the call's typed
    // `.frames`, read back by call_id from the harness-owned store (the
    // chamber-unforgeable half — separate PVC + RBAC — is infrastructure, not
    // unit-testable).
    //
    // Materiality: a mutant that drops the stderr frame before persisting, or
    // `append_frame` overwriting instead of appending, reds the stderr assertion —
    // the exact information the execution log exists to keep. A mutant that
    // persists without keying by call_id reds the read-back for `call-both`.
    #[tokio::test]
    async fn execution_record_captures_both_stdout_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf(), "test-conv".to_string());
        for frame in [
            stdout_f("recorded stdout A"),
            stderr_f("recorded stderr B"),
            complete_f(false, 0),
        ] {
            log.append_frame("call-both", &frame)
                .await
                .expect("append frame");
        }
        let persisted = log.read("call-both").await.expect("persisted record");
        let has_stdout = persisted.frames().iter().any(
            |f| matches!(f.frame.as_ref(), Some(Frame::Stdout(s)) if s.contains("recorded stdout A")),
        );
        let has_stderr = persisted.frames().iter().any(
            |f| matches!(f.frame.as_ref(), Some(Frame::Stderr(s)) if s.contains("recorded stderr B")),
        );
        assert!(has_stdout, "the persisted record keeps stdout");
        assert!(has_stderr, "the persisted record keeps stderr");
    }

    // A tool call's output frames append as ND-JSON lines to the conversation's
    // single execution.json, one record per frame, each self-identifying by
    // call_id.
    //
    // Materiality: a store that writes one `exec-<call_id>.frames` file per call
    // never creates `execution.json`, reding the exists assertion.
    // A mutant that overwrites instead of appends (one record survives) reds the
    // two-record count; a mutant that drops `call_id` from each record reds the
    // call_id-substring assertion; prost/other non-JSON framing reds the
    // per-line JSON parse.
    //
    // Pins the on-disk shape (single ND-JSON file, one record per appended
    // frame, each self-identifying by call_id). It fails against an empty impl (no
    // file) and against the current impl (wrong file, wrong framing).
    #[tokio::test]
    async fn model_tool_frames_are_appended_as_ndjson_lines_to_execution_json() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf(), "test-conv".to_string());

        log.append_frame("call-1", &stdout_f("hello-from-call-1"))
            .await
            .expect("append stdout frame");
        log.append_frame("call-1", &complete_f(false, 0))
            .await
            .expect("append terminal frame");

        let exec_json = dir.path().join("execution.json");
        assert!(
            exec_json.is_file(),
            "the conversation's frames must land in a single execution.json, none at {}",
            exec_json.display()
        );

        let records = ndjson_records(&exec_json);
        assert_eq!(
            records.len(),
            2,
            "one appended frame -> one ND-JSON record; append (not overwrite) keeps both, got {records:?}"
        );
        for line in &records {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("each execution.json line must be JSON, {line:?}: {e}"));
        }
        let text = std::fs::read_to_string(&exec_json).unwrap();
        assert!(
            text.contains("call-1"),
            "each line carries its call_id so a single file can hold many calls, got {text:?}"
        );
    }

    // Two tool calls writing concurrently to one conversation's execution.json
    // serialize their appends so no line interleaves.
    //
    // Materiality: a store that writes a separate file per call never creates
    // execution.json, reding the exists assertion. A mutant
    // that drops the per-conversation serialization mutex lets two large
    // concurrent `write_all`s interleave mid-line, producing at least one record
    // that fails to parse as JSON — the per-line parse assertion reds. Losing a
    // call's records to a clobbering write reds the per-call replay counts.
    //
    // Pins that one shared execution.json with two concurrent writers stays
    // line-atomic — the exact guarantee the mutex exists to provide. Large
    // payloads force multi-syscall writes so the unguarded path corrupts
    // observably.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_to_one_conversation_do_not_interleave() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(LocalFsExecutionLog::new(
            dir.path().to_path_buf(),
            "test-conv".to_string(),
        ));

        const PER_CALL: usize = 30;
        let payload_a = "A".repeat(16 * 1024);
        let payload_b = "B".repeat(16 * 1024);

        let task_a = {
            let log = log.clone();
            tokio::spawn(async move {
                for _ in 0..PER_CALL {
                    log.append_frame("call-A", &stdout_f(&payload_a))
                        .await
                        .expect("append A");
                }
            })
        };
        let task_b = {
            let log = log.clone();
            tokio::spawn(async move {
                for _ in 0..PER_CALL {
                    log.append_frame("call-B", &stdout_f(&payload_b))
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

    // A log file with a half-written trailing line is read by skipping that line
    // rather than failing; the intact records before the tear still replay.
    //
    // Materiality: a reader that looks at `exec-<call_id>.frames` never consults
    // the execution.json this test corrupts, so the exists guard reds. A mutant
    // that fails the whole read on a bad line (an early `?`/return-None on parse
    // error) drops the valid
    // records too — the "good frames survive" assertions red. A mutant that lets
    // the panic propagate crashes the read.
    //
    // Pins tolerance of a torn tail by asserting the records before the tear are
    // still returned. The good records are written through the real API so the
    // test does not hard-code the record schema.
    #[tokio::test]
    async fn reader_skips_a_half_written_trailing_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf(), "test-conv".to_string());

        log.append_frame("call-tol", &stdout_f("good-line-before-the-tear"))
            .await
            .expect("append stdout");
        log.append_frame("call-tol", &complete_f(false, 0))
            .await
            .expect("append terminal");

        let exec_json = dir.path().join("execution.json");
        assert!(
            exec_json.is_file(),
            "records must be persisted to execution.json before the tolerance check, none at {}",
            exec_json.display()
        );

        // Simulate a crash mid-append: a partial JSON fragment with no newline.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&exec_json)
                .unwrap();
            f.write_all(b"{\"call_id\":\"call-tol\",\"frame\":\"stdo")
                .unwrap();
        }

        let persisted = log
            .read("call-tol")
            .await
            .expect("a torn trailing line must not make the read fail");
        assert!(
            persisted
                .frames()
                .iter()
                .any(|f| stdout_text(f) == Some("good-line-before-the-tear")),
            "the intact record before the tear is still replayed"
        );
        assert!(
            persisted.frames().iter().any(is_terminal),
            "the intact terminal before the tear is still replayed"
        );
    }

    // A re-subscribe to a call by id replays only that call's frames, filtered by
    // call_id from a shared execution.json holding several calls.
    //
    // Materiality: a store that keeps one file per call never creates a single
    // execution.json holding both calls, reding the single-file assertion. A
    // mutant that reads the whole file without filtering by call_id returns the
    // other call's frames too —
    // the per-call length and the "does not contain the other marker" assertions
    // red.
    //
    // Frames for two calls are interleaved in one file, and the test pins that
    // read(call) returns exactly one call's frames; a no-op filter fails this.
    #[tokio::test]
    async fn resubscribe_replays_a_call_filtered_from_a_shared_execution_json() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf(), "test-conv".to_string());

        // Two calls interleaved in one conversation's execution.json.
        log.append_frame("call-A", &stdout_f("AAA-marker"))
            .await
            .unwrap();
        log.append_frame("call-B", &stdout_f("BBB-marker"))
            .await
            .unwrap();
        log.append_frame("call-A", &complete_f(false, 0))
            .await
            .unwrap();
        log.append_frame("call-B", &complete_f(false, 0))
            .await
            .unwrap();

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

    // AC1 (no artifact bytes in the crash log) + AC3 (no pointer/reference in the
    // crash log). A produced-artifact image frame persists nothing: no
    // content-addressed blob is written, and execution.json carries neither the
    // image's digest nor an image record line referencing it. Only
    // stdout/stderr/terminal are the crash log's business.
    //
    // Materiality: reds against the current impl, which writes the image bytes to
    // blobs/sha256/<hex> and appends an Image record carrying that digest
    // (frame_to_record's Image arm + write_blob). The coder's edit makes the Image
    // arm persist nothing. A regression re-adding the blob write reds the no-blob
    // assertion; re-adding the digest record reds the no-digest / no-image-record
    // assertions. Inverse of
    // `binary_frames_are_stored_as_content_addressed_blobs_referenced_by_digest`.
    #[tokio::test]
    async fn a_produced_image_frame_persists_no_bytes_and_no_reference_in_the_crash_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf(), "conv-img".to_string());

        // A distinctive, sizable payload: any leaked byte or digest is plainly
        // visible in the on-disk crash log.
        let bytes: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let hex = sha256_hex_bytes(&bytes);

        log.append_frame("call-img", &image_f("image/png", bytes.clone()))
            .await
            .expect("append image frame");
        log.append_frame("call-img", &complete_f(false, 0))
            .await
            .expect("append terminal frame");

        // (a) No content-addressed blob is written for the produced image.
        let blobs = dir.path().join("blobs");
        assert!(
            !blobs.exists(),
            "a produced-artifact image writes NO bytes to the crash log's blob tree; found {}",
            blobs.display()
        );

        // (b) execution.json references no image digest.
        let exec_json = dir.path().join("execution.json");
        let text = std::fs::read_to_string(&exec_json).unwrap_or_default();
        assert!(
            !text.contains(&hex),
            "the crash log references no image digest, got {text:?}"
        );

        // (c) No image record line is persisted; only the terminal survives.
        let records = ndjson_records(&exec_json);
        assert!(
            !records.iter().any(|l| l.contains("\"frame\":\"image\"")),
            "no image record line is persisted for a produced artifact, got {records:?}"
        );
    }

    // AC4: while a chamber streams stdout and stderr, those frames continue to be
    // recorded frame-by-frame — even interleaved with a produced-artifact image
    // frame whose bytes are dropped. Dropping the image must not disturb the
    // surrounding stdout/stderr records.
    //
    // Materiality: green now and after the fix. A too-broad edit that made
    // frame_to_record return None for the whole stream, or dropped the frame
    // adjacent to the image, reds the stdout/stderr read-backs. Complements
    // `execution_record_captures_both_stdout_and_stderr` by placing an image
    // between the two streamed frames.
    #[tokio::test]
    async fn stdout_and_stderr_around_a_dropped_image_frame_still_stream() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf(), "conv-interleave".to_string());

        for frame in [
            stdout_f("stdout before the image"),
            image_f("image/png", vec![9, 8, 7, 6]),
            stderr_f("stderr after the image"),
            complete_f(false, 0),
        ] {
            log.append_frame("call-mix", &frame)
                .await
                .expect("append frame");
        }

        let persisted = log.read("call-mix").await.expect("persisted record");
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

    // execution_log.rs:200 `blob_dir` — blobs are rooted at
    // `<conversation root>/blobs/sha256`. Materiality: change either path
    // segment ("blobs"/"sha256"), the join order, or the root the dir hangs
    // off, and the equality reds.
    #[test]
    fn blob_dir_is_rooted_under_the_conversation_root() {
        let root = std::path::PathBuf::from("/tmp/some-conversation-root");
        let log = LocalFsExecutionLog::new(root.clone(), "conv-1".to_string());
        assert_eq!(log.blob_dir(), root.join("blobs").join("sha256"));
    }
}

// Tests for the append-as-you-go execution log's replay read API: frames
// appended one at a time read back in arrival order, with the terminal's
// presence reported, so a dropped result stream replays from the store without
// re-running the tool.
#[cfg(test)]
mod replay_tests {
    use super::*;
    use proto_common::tool_result_frame::Frame;
    use proto_common::{ToolComplete, ToolOutcome, ToolResultFrame};

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
    fn canceled_terminal() -> ToolResultFrame {
        ToolResultFrame {
            frame: Some(Frame::Complete(ToolComplete {
                outcome: ToolOutcome::Canceled as i32,
                exit_code: -1,
            })),
        }
    }
    fn is_complete(frame: &ToolResultFrame) -> bool {
        matches!(frame.frame.as_ref(), Some(Frame::Complete(_)))
    }

    // Frames appended one at a time are read back in arrival order, every
    // pre-terminal frame included, with the terminal flagged present. This is
    // the "served from its persisted record" half of re-subscribe: the record
    // holds the whole stream, not just the last frame.
    //
    // Materiality: `append_frame` overwriting instead of appending (the current
    // `tokio::fs::write` full-overwrite behavior) leaves only the terminal, so
    // the length-3 and stdout/stderr assertions red. `read` returning only the
    // terminal (dropping pre-terminal frames) reds them the same way. Losing
    // arrival order reds the frames[0]/frames[1] position assertions.
    #[tokio::test]
    async fn read_returns_appended_frames_in_order_with_terminal_present() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf(), "test-conv".to_string());

        log.append_frame("call-replay", &stdout_f("first stdout"))
            .await
            .expect("append stdout frame");
        log.append_frame("call-replay", &stderr_f("second stderr"))
            .await
            .expect("append stderr frame");
        log.append_frame("call-replay", &canceled_terminal())
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
        assert!(is_complete(&frames[2]), "the terminal frame replays last");
        assert!(
            persisted.has_terminal(),
            "a record whose last frame is a ToolComplete reports the terminal present"
        );
    }

    // A record dropped mid-call — frames persisted but no terminal
    // appended — reports its terminal absent, so the replay path can distinguish
    // a finished record from a truncated one (the session died before the
    // runtime's terminal). Complement to the terminal-present case above, which
    // only exercises the `true` branch.
    //
    // Materiality: forcing `has_terminal` to always return `true` reds this — a
    // truncated record
    // would wrongly report complete. The length guard pins that the `false` is
    // owed to the missing terminal, not to an empty read.
    #[tokio::test]
    async fn read_of_a_record_without_a_terminal_reports_no_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf(), "test-conv".to_string());

        log.append_frame("call-truncated", &stdout_f("mid-call stdout"))
            .await
            .expect("append stdout frame");
        log.append_frame("call-truncated", &stderr_f("mid-call stderr"))
            .await
            .expect("append stderr frame");

        let persisted = log
            .read("call-truncated")
            .await
            .expect("a truncated record still reads back");
        assert_eq!(
            persisted.frames().len(),
            2,
            "both non-terminal frames persisted, so the record is genuinely populated"
        );
        assert!(
            !persisted.has_terminal(),
            "a record whose last frame is not a ToolComplete reports the terminal absent"
        );
    }

    // A `call_id` with no persisted record reads as absent — the replay
    // path reports "nothing here" rather than fabricating an empty finished
    // call.
    //
    // Materiality: `read` returning `Some(empty)` for an unknown id (so a
    // re-subscribe to a never-run call looks like a finished empty call) reds
    // the `is_none` assertion.
    #[tokio::test]
    async fn read_of_an_unknown_call_id_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let log = LocalFsExecutionLog::new(dir.path().to_path_buf(), "test-conv".to_string());
        let persisted = log.read("never-appended").await;
        assert!(
            persisted.is_none(),
            "an unknown call_id has no persisted record to replay"
        );
    }
}
