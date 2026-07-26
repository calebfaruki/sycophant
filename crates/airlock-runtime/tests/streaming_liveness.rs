//! The runtime streams output as it runs, not after the child exits.
//!
//! A frame must leave the producer while the child is still running. The pure
//! buffer-to-frames seam operates on already-complete buffers and cannot witness
//! this — it passes trivially for a buffer-then-slice implementation. This test
//! exercises the streaming boundary, `execute::stream_frames`, and fails unless
//! frames are emitted incrementally.

use airlock_proto::tool_result_frame::Frame;
use airlock_runtime::execute::stream_frames;
use shared::scrub::ScrubSet;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn no_scrub() -> ScrubSet {
    ScrubSet::from_env_var("__UNSET_LIVENESS_TEST_SCRUB__")
}

// A stdout frame is emitted while the child is still running.
//
// The child writes+flushes one stdout line, then blocks in `sleep` WITHOUT
// exiting or closing its stdout. A buffer-then-emit producer (`read_to_end`,
// then `frames_for`, then send) cannot emit anything until stdout hits EOF,
// which the still-alive child defers for the whole sleep — so no frame arrives
// within the timeout and the `recv` expect reds. An incremental producer
// forwards the line the instant the reader yields it, so the frame arrives in
// milliseconds, well before the child terminates.
//
// Materiality: reverting the producer to buffer-then-emit — emitting frames
// only after `child.wait()` / `read_to_end` rather than per line as chunks
// arrive — defers the first frame past the child's exit, timing out `recv` and
// reding this test. (The shipped placeholder body of `stream_frames` is exactly
// that buffered form, so this test is RED against the current code.)
#[tokio::test]
async fn a_stdout_frame_is_emitted_while_the_child_is_still_running() {
    // `sh`'s `echo` builtin write()s the line to the pipe and flushes before the
    // next command runs; `sleep 30` then holds the process — and its open
    // stdout — alive so a buffered reader cannot reach EOF.
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg("echo first line; sleep 30");

    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let producer_cancel = cancel.clone();
    let scrub = no_scrub();
    let handle = tokio::spawn(async move {
        stream_frames(cmd, &producer_cancel, None, &scrub, tx).await;
    });

    let first = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect(
            "a stdout frame must arrive WHILE the child is still running (incremental \
             streaming), not only after the child exits",
        )
        .expect("the producer must send at least one frame before EOF");

    match first.frame {
        Some(Frame::Stdout(s)) => assert!(
            s.contains("first line"),
            "the first streamed frame carries the child's first stdout line, got {s:?}"
        ),
        other => panic!(
            "the first streamed frame must be the stdout line the child already wrote, \
             got {other:?}"
        ),
    }

    // The frame was observed mid-run: the producer is still supervising the
    // running child (blocked in `sleep 30`), not finished. A post-exit buffered
    // emit could only have reached this line after the 30s sleep, which the 3s
    // timeout above already excluded.
    assert!(
        !handle.is_finished(),
        "the stdout frame was observed before the child terminated — the producer must \
         still be supervising the running child"
    );

    // Cleanup: fire the cancel cascade so the child is SIGKILLed and reaped and
    // the producer returns, rather than leaving `sleep 30` running.
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}
