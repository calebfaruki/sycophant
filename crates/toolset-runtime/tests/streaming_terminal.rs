//! Terminal-frame behavior of the incremental streaming producer.
//!
//! Every `stream_frames` run ends in exactly one `ToolComplete`. These tests
//! drive real child processes through the producer and read the whole frame
//! stream to that terminal, pinning the exit code and error flag the terminal
//! carries for each way a child can end: a normal zero exit, a normal non-zero
//! exit, a cancel, a timeout, a self-signalled death, a failed spawn, and an
//! image reference that could not be assembled. A separate case pins the loop
//! guard: when one pipe reaches EOF while the other keeps emitting, the still
//! open pipe's later lines must still stream.

use proto_common::tool_result_frame::Frame;
use proto_common::{ToolComplete, ToolOutcome, ToolResultFrame};
use shared::scrub::ScrubSet;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use toolset_runtime::execute::stream_frames;

fn no_scrub() -> ScrubSet {
    ScrubSet::from_env_var("__UNSET_TERMINAL_TEST_SCRUB__")
}

fn last_complete(frames: &[ToolResultFrame]) -> &ToolComplete {
    match frames.last().and_then(|f| f.frame.as_ref()) {
        Some(Frame::Complete(c)) => c,
        other => panic!("the last frame must be the terminal ToolComplete, got {other:?}"),
    }
}

fn stdout_text(frames: &[ToolResultFrame]) -> String {
    frames
        .iter()
        .filter_map(|f| match f.frame.as_ref() {
            Some(Frame::Stdout(s)) => Some(s.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn stderr_text(frames: &[ToolResultFrame]) -> String {
    frames
        .iter()
        .filter_map(|f| match f.frame.as_ref() {
            Some(Frame::Stderr(s)) => Some(s.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run the producer to its terminal, draining every frame it emits. The
/// producer owns the sender, so its return drops the sender and ends the drain.
async fn collect(
    cmd: tokio::process::Command,
    cancel: &CancellationToken,
    timeout: Option<Duration>,
    scrub: &ScrubSet,
) -> Vec<ToolResultFrame> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    tokio::time::timeout(
        Duration::from_secs(10),
        stream_frames(cmd, cancel, timeout, scrub, tx),
    )
    .await
    .expect("stream_frames must reach its terminal and return");
    let mut frames = Vec::new();
    while let Some(f) = rx.recv().await {
        frames.push(f);
    }
    frames
}

fn sh(script: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(script);
    cmd
}

// A child that exits non-zero terminates the stream with a ToolComplete carrying
// that exact exit code and flagged as an error.
//
// Materiality: the terminal error flag is `exit_code != 0 || image_error`.
// Flipping `!=` to `==` makes a non-zero exit read as non-error (`is_error`
// reds); flipping the `||` to `&&` drops the exit-code term entirely (with no
// image error, `is_error` reds). Losing the real exit code (e.g. the terminal
// hardcoding 0) reds the `exit_code == 7` assertion.
#[tokio::test]
async fn non_zero_exit_terminal_carries_the_code_and_is_error() {
    let cancel = CancellationToken::new();
    let frames = collect(sh("exit 7"), &cancel, None, &no_scrub()).await;
    let c = last_complete(&frames);
    assert_eq!(
        c.exit_code, 7,
        "the terminal carries the child's real exit code"
    );
    assert_ne!(
        c.outcome(),
        ToolOutcome::Done,
        "a non-zero exit flags the terminal as an error"
    );
}

// A child that exits zero terminates the stream with a non-error ToolComplete
// carrying exit code 0.
//
// Materiality: this pins the success branch of `exit_code != 0 || image_error`.
// A terminal that hardcodes a non-zero/`-1` exit reds `exit_code == 0`; one that
// always flags an error reds `!is_error`.
#[tokio::test]
async fn zero_exit_terminal_is_not_an_error() {
    let cancel = CancellationToken::new();
    let frames = collect(sh("exit 0"), &cancel, None, &no_scrub()).await;
    let c = last_complete(&frames);
    assert_eq!(c.exit_code, 0, "a clean exit carries exit code 0");
    assert_eq!(
        c.outcome(),
        ToolOutcome::Done,
        "a zero exit is not flagged as an error"
    );
}

// Cancelling a blocked child mid-run terminates the stream with the killed
// sentinel: exit code -1 and an error terminal.
//
// Materiality: the terminal reads `if aborted { -1 } else { .. }`. A mutant that
// changes the aborted sentinel away from -1 (e.g. to 0) reds `exit_code == -1`
// and, because a zero exit is not an error, reds `is_error` too.
#[tokio::test]
async fn cancelled_run_terminal_is_the_killed_sentinel() {
    let cancel = CancellationToken::new();
    let fire = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        fire.cancel();
    });
    let frames = collect(sh("sleep 30"), &cancel, None, &no_scrub()).await;
    let c = last_complete(&frames);
    assert_eq!(
        c.exit_code, -1,
        "a cancel-killed child reports the -1 sentinel, not a real exit code"
    );
    assert_eq!(
        c.outcome(),
        ToolOutcome::Canceled,
        "a cancel-killed run terminates as CANCELED"
    );
}

// A run that overruns its deadline terminates with the killed sentinel and a
// timeout notice on stderr.
//
// Materiality: exit code -1 pins the aborted (timed-out) sentinel branch;
// requiring the "timed out" stderr frame pins the timeout-notice emission —
// deleting that block leaves no such frame and reds the stderr assertion.
#[tokio::test]
async fn timed_out_run_terminal_is_the_killed_sentinel_with_a_notice() {
    let cancel = CancellationToken::new();
    let frames = collect(
        sh("sleep 30"),
        &cancel,
        Some(Duration::from_millis(100)),
        &no_scrub(),
    )
    .await;
    let c = last_complete(&frames);
    assert_eq!(c.exit_code, -1, "a timed-out child reports the -1 sentinel");
    assert_eq!(
        c.outcome(),
        ToolOutcome::Failed,
        "a timed-out run terminates as FAILED"
    );
    assert!(
        stderr_text(&frames).contains("timed out"),
        "the timeout path emits a stderr notice, got {:?}",
        stderr_text(&frames)
    );
}

// A child that signals its own death (no cancel, no timeout) has no exit code;
// the terminal falls back to the -1 sentinel.
//
// Materiality: with `aborted` false, the terminal is
// `status.and_then(|s| s.code()).unwrap_or(-1)`. A signal-killed child yields
// `code() == None`, so the fallback is the only source of the value. Changing
// the `unwrap_or(-1)` default (e.g. to 0) reds `exit_code == -1`.
#[tokio::test]
async fn signal_killed_child_terminal_falls_back_to_minus_one() {
    let cancel = CancellationToken::new();
    let frames = collect(sh("kill -9 $$"), &cancel, None, &no_scrub()).await;
    let c = last_complete(&frames);
    assert_eq!(
        c.exit_code, -1,
        "a signal death has no exit code, so the terminal falls back to -1"
    );
    assert_ne!(
        c.outcome(),
        ToolOutcome::Done,
        "a signal-killed run terminates as an error"
    );
}

// A command that cannot be spawned terminates with an error terminal at the -1
// sentinel and a scrubbed stdout error frame instead of any tool output.
//
// Materiality: the spawn-failure path emits `ToolComplete { is_error: true,
// exit_code: -1 }`. Changing that sentinel exit code (e.g. to 0) reds
// `exit_code == -1`; dropping the error frame reds the "execution error"
// assertion.
#[tokio::test]
async fn spawn_failure_terminal_is_the_error_sentinel() {
    let cancel = CancellationToken::new();
    let mut cmd = tokio::process::Command::new("/no/such/dispatch-binary-xyzzy");
    cmd.arg("noop");
    let frames = collect(cmd, &cancel, None, &no_scrub()).await;
    let c = last_complete(&frames);
    assert_eq!(
        c.exit_code, -1,
        "an unspawnable command terminates at the -1 sentinel"
    );
    assert_ne!(
        c.outcome(),
        ToolOutcome::Done,
        "a spawn failure terminates as an error"
    );
    assert!(
        stdout_text(&frames).contains("execution error"),
        "the spawn failure surfaces a scrubbed error frame, got {:?}",
        stdout_text(&frames)
    );
}

// An unassemblable image reference on stdout flags the terminal as an error even
// when the child exits zero.
//
// Materiality: an image-marker line whose scratch file is unreadable sets the
// producer's `image_error` flag, which the terminal folds in via
// `exit_code != 0 || image_error`. Dropping the `image_error = true` assignment,
// or replacing the `||` with `&&`, leaves the zero-exit terminal non-error and
// reds `is_error`.
#[tokio::test]
async fn unassemblable_image_marker_flags_the_terminal_error_on_zero_exit() {
    let cancel = CancellationToken::new();
    // Emit an image-marker line (US-delimited) pointing at a path that does not
    // exist, then exit clean. The producer cannot read the scratch file, so it
    // marks an image error without the child failing.
    let frames = collect(
        sh("printf '\\037AIRLOCK-IMAGE\\037image/png\\037/no/such/scratch.png\\n'; exit 0"),
        &cancel,
        None,
        &no_scrub(),
    )
    .await;
    let c = last_complete(&frames);
    assert_eq!(c.exit_code, 0, "the child itself exited clean");
    assert_ne!(
        c.outcome(),
        ToolOutcome::Done,
        "an unassemblable image reference flags the terminal as an error despite the zero exit"
    );
}

// When stdout reaches EOF while stderr keeps emitting, the still-open stderr
// pipe's later lines still stream — the loop follows either pipe, not both.
//
// Materiality: the pump loop runs `while stdout_open || stderr_open`. Flipping
// the `||` to `&&` exits the loop the instant stdout EOFs, dropping every stderr
// line the child writes afterward — the "err-after-stdout-eof" frame never
// arrives and this test reds.
#[tokio::test]
async fn asymmetric_pipe_eof_still_streams_the_open_pipe() {
    let cancel = CancellationToken::new();
    // echo to stdout, close stdout, pause, then write to stderr. stdout EOFs
    // well before stderr produces its line.
    let frames = collect(
        sh("echo out; exec 1>&-; sleep 0.2; echo err 1>&2"),
        &cancel,
        None,
        &no_scrub(),
    )
    .await;
    assert!(
        stdout_text(&frames).contains("out"),
        "the early stdout line streams, got {:?}",
        stdout_text(&frames)
    );
    assert!(
        stderr_text(&frames).contains("err"),
        "the stderr line written after stdout EOF must still stream, got {:?}",
        stderr_text(&frames)
    );
}
