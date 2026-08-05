use std::collections::HashMap;

use proto_common::tool_result_frame::Frame;
use proto_common::{ToolOutcome, ToolResultFrame};
use shared::scrub::ScrubSet;
use tokio_util::sync::CancellationToken;

pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    /// `Some(sig)` when the child was terminated by a signal (e.g. `9` for the
    /// cancel-driven SIGKILL) rather than exiting normally. Distinguishes a
    /// killed call from a normal exit that happens to be non-zero.
    pub terminated_by_signal: Option<i32>,
}

/// Spawn a reader task draining a piped child handle to end. Reading the pipes
/// in tasks (rather than `wait_with_output`, which consumes the child) is what
/// lets us keep the `Child` to kill on cancel while still collecting output.
pub(crate) fn spawn_reader<R>(handle: Option<R>) -> tokio::task::JoinHandle<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        if let Some(mut h) = handle {
            let _ = h.read_to_end(&mut buf).await;
        }
        buf
    })
}

/// The trusted runtime spawning and supervising a model-authored child under
/// cancellation. Pipes the child's output, then races its exit against the
/// turn's cancellation (and an optional deadline) in one `biased` `select!`:
/// if either fires, SIGKILL and reap the retained child. Reports how it ended
/// via `CommandResult`, with `terminated_by_signal` set for a killed child.
/// Output is drained by reader tasks taken before the race, since
/// `wait_with_output` would consume the child and preclude a kill.
pub(crate) async fn run_supervised_child(
    mut cmd: tokio::process::Command,
    cancel: &CancellationToken,
    timeout: Option<std::time::Duration>,
) -> Result<CommandResult, std::io::Error> {
    use std::os::unix::process::ExitStatusExt;

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;

    let stdout_task = spawn_reader(child.stdout.take());
    let stderr_task = spawn_reader(child.stderr.take());

    enum Outcome {
        Exited(std::process::ExitStatus),
        Killed(Option<std::process::ExitStatus>),
        TimedOut,
    }

    // A missing deadline pends forever, collapsing the race to cancel vs exit.
    let deadline = async {
        match timeout {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending::<()>().await,
        }
    };

    let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let _ = child.start_kill();
            Outcome::Killed(child.wait().await.ok())
        }
        _ = deadline => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Outcome::TimedOut
        }
        res = child.wait() => Outcome::Exited(res?),
    };

    let stdout = String::from_utf8_lossy(&stdout_task.await.unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_task.await.unwrap_or_default()).into_owned();

    Ok(match outcome {
        Outcome::Exited(status) => CommandResult {
            stdout,
            stderr,
            exit_code: status.code().unwrap_or(-1),
            terminated_by_signal: status.signal(),
        },
        Outcome::Killed(status) => CommandResult {
            stdout,
            stderr,
            exit_code: -1,
            terminated_by_signal: status.and_then(|s| s.signal()).or(Some(9)),
        },
        // Timeout returns the same -1 sentinel a signal-killed process does, so
        // downstream callers route both as "abnormal exit".
        Outcome::TimedOut => CommandResult {
            stdout: String::new(),
            stderr: format!(
                "command timed out after {}s",
                timeout.map(|d| d.as_secs()).unwrap_or(0)
            ),
            exit_code: -1,
            terminated_by_signal: None,
        },
    })
}

/// Toolset-provided dispatcher path. Convention: every toolset image places
/// an executable here. Toolset spawns it with `argv = [tool_name]`, env =
/// arg values (one env var per declared arg, named per the schema), and
/// cwd = working_dir. The dispatcher decides how to route — Makefile, case
/// statement, Python script, native binary — entirely the toolset author's
/// choice. Not LLM-derived, never overridable per call.
pub const TOOLSET_DISPATCH: &str = "/etc/toolset/dispatch";

/// Build the dispatcher invocation for a tool call. argv is exactly
/// `[TOOLSET_DISPATCH, tool_name]`. Arg values flow in via environment
/// variables — never as `KEY=val` argv positions, which would let make-style
/// dispatchers smuggle the value into recipe text before the shell parses
/// it.
///
/// Pure: no spawning, no I/O. Returns a configured `tokio::process::Command`
/// for the caller to spawn, or for tests to inspect.
pub fn compose_dispatch_command(
    tool_name: &str,
    args: &HashMap<String, String>,
    working_dir: &str,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(TOOLSET_DISPATCH);
    cmd.arg(tool_name);
    cmd.current_dir(working_dir);
    for (env_name, val) in args {
        cmd.env(env_name, val);
    }
    cmd
}

/// Streaming frame producer seam. Spawns and
/// supervises `cmd`, turning its output into the ordered typed-frame stream and
/// forwarding each frame to `tx` **as the child produces it** — a stdout line is
/// observable on `tx` while the child is still running, not only after it exits.
/// The stdout and stderr pipes are pumped line-by-line and concurrently, each
/// line forwarded the instant the reader yields it. Per-line marker parsing and
/// scrubbing follow [`crate::parts`]; stdout and stderr stay distinct typed
/// frames; a single terminal [`ToolComplete`] closes the stream. The child stays
/// supervised: a fired `cancel` (or the optional `timeout`) SIGKILLs it, which
/// EOFs the pipes and drains the readers, and the terminal reports
/// `exit_code: -1` for a killed/timed-out child (same sentinel as
/// [`run_supervised_child`]); already-forwarded frames remain.
pub async fn stream_frames(
    mut cmd: tokio::process::Command,
    cancel: &CancellationToken,
    timeout: Option<std::time::Duration>,
    scrub: &ScrubSet,
    tx: tokio::sync::mpsc::Sender<ToolResultFrame>,
) {
    use tokio::io::AsyncBufReadExt;

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx
                .send(ToolResultFrame {
                    frame: Some(Frame::Stdout(scrub.apply(&format!("execution error: {e}")))),
                })
                .await;
            let _ = tx
                .send(crate::parts::complete_frame(ToolOutcome::Failed, -1))
                .await;
            return;
        }
    };

    let mut stdout_lines = child
        .stdout
        .take()
        .map(|h| tokio::io::BufReader::new(h).lines());
    let mut stderr_lines = child
        .stderr
        .take()
        .map(|h| tokio::io::BufReader::new(h).lines());
    let mut stdout_open = stdout_lines.is_some();
    let mut stderr_open = stderr_lines.is_some();

    // A missing deadline pends forever, collapsing the race to cancel vs exit.
    let deadline = async {
        match timeout {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(deadline);

    let mut image_error = false;
    let mut aborted = false;
    let mut timed_out = false;

    // Pump both pipes concurrently, forwarding each line as a frame the instant
    // it arrives, while racing the turn cancel and the optional deadline. On
    // cancel/timeout we SIGKILL the child, which EOFs the pipes so the readers
    // drain and the loop ends.
    while stdout_open || stderr_open {
        tokio::select! {
            biased;
            _ = cancel.cancelled(), if !aborted => {
                let _ = child.start_kill();
                aborted = true;
            }
            _ = &mut deadline, if !aborted => {
                let _ = child.start_kill();
                aborted = true;
                timed_out = true;
            }
            line = next_line(&mut stdout_lines), if stdout_open => match line {
                Some(l) => {
                    let (frame, err) = crate::parts::stdout_line_frame(&l, scrub);
                    if err {
                        image_error = true;
                    }
                    if let Some(f) = frame {
                        if tx.send(f).await.is_err() {
                            return;
                        }
                    }
                }
                None => stdout_open = false,
            },
            line = next_line(&mut stderr_lines), if stderr_open => match line {
                Some(l) => {
                    if tx.send(crate::parts::stderr_line_frame(&l, scrub)).await.is_err() {
                        return;
                    }
                }
                None => stderr_open = false,
            },
        }
    }

    // Reap the child (killed or exited) and read its real exit code. A
    // killed/timed-out child reports the -1 sentinel, matching
    // `run_supervised_child`.
    let status = child.wait().await.ok();
    let exit_code = if aborted {
        -1
    } else {
        status.and_then(|s| s.code()).unwrap_or(-1)
    };

    if timed_out {
        let msg = format!(
            "command timed out after {}s",
            timeout.map(|d| d.as_secs()).unwrap_or(0)
        );
        let _ = tx.send(crate::parts::stderr_line_frame(&msg, scrub)).await;
    }

    // Cancel, timeout, and a genuine failure reach distinct terminals. A fired
    // cancel that was not the deadline (`aborted && !timed_out`) is the user
    // cancel — CANCELED, not an error, even though the killed child reports the
    // -1 sentinel. A timeout folds into FAILED, as does a non-zero exit or a
    // failed image reference; a clean exit is DONE.
    let outcome = if aborted && !timed_out {
        ToolOutcome::Canceled
    } else if timed_out || exit_code != 0 || image_error {
        ToolOutcome::Failed
    } else {
        ToolOutcome::Done
    };
    let _ = tx
        .send(crate::parts::complete_frame(outcome, exit_code))
        .await;
}

/// Await the next line from an optional line reader. `None` marks EOF (or a read
/// error, treated as EOF); an absent reader pends forever so its `select!` arm —
/// guarded off — is never actually polled.
async fn next_line<R>(lines: &mut Option<tokio::io::Lines<R>>) -> Option<String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    match lines {
        Some(l) => l.next_line().await.ok().flatten(),
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn argv_strings(cmd: &tokio::process::Command) -> Vec<String> {
        let std_cmd = cmd.as_std();
        std::iter::once(std_cmd.get_program())
            .chain(std_cmd.get_args())
            .map(|s: &OsStr| s.to_string_lossy().into_owned())
            .collect()
    }

    fn env_pairs(cmd: &tokio::process::Command) -> HashMap<String, String> {
        cmd.as_std()
            .get_envs()
            .filter_map(|(k, v)| {
                v.map(|val| {
                    (
                        k.to_string_lossy().into_owned(),
                        val.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect()
    }

    #[test]
    fn compose_argv_is_dispatch_then_tool_name() {
        let cmd = compose_dispatch_command("notion-search", &HashMap::new(), "/workspace");
        assert_eq!(
            argv_strings(&cmd),
            vec!["/etc/toolset/dispatch", "notion-search"]
        );
    }

    #[test]
    fn compose_sets_working_dir() {
        let cmd = compose_dispatch_command("t", &HashMap::new(), "/some/dir");
        assert_eq!(
            cmd.as_std().get_current_dir().map(|p| p.to_str().unwrap()),
            Some("/some/dir")
        );
    }

    #[test]
    fn compose_sets_env_vars_from_args() {
        let mut args = HashMap::new();
        args.insert("QUERY".into(), "hello".into());
        args.insert("PAGE_ID".into(), "abc-123".into());
        let cmd = compose_dispatch_command("t", &args, "/w");
        let env = env_pairs(&cmd);
        assert_eq!(env.get("QUERY"), Some(&"hello".to_string()));
        assert_eq!(env.get("PAGE_ID"), Some(&"abc-123".to_string()));
    }

    #[test]
    fn compose_special_chars_pass_verbatim_in_env() {
        // Security-critical: special chars in values must reach the
        // toolset dispatcher unchanged. The dispatcher (Makefile,
        // shell script, etc.) is responsible for safe quoting via
        // `"$VAR"` shell expansion, which is a single argv token
        // regardless of contents.
        let mut args = HashMap::new();
        args.insert("Q".into(), r#"foo"; rm -rf /; #"#.into());
        let cmd = compose_dispatch_command("t", &args, "/w");
        assert_eq!(
            env_pairs(&cmd).get("Q"),
            Some(&r#"foo"; rm -rf /; #"#.to_string())
        );
    }

    #[test]
    fn compose_no_kv_argv_positions() {
        // Args must NEVER appear as `KEY=val` argv elements — that would
        // let a make-style dispatcher parse them as variable overrides
        // and expand them into recipe text before the shell sees them.
        // Only env vars.
        let mut args = HashMap::new();
        args.insert("QUERY".into(), "evil".into());
        let cmd = compose_dispatch_command("t", &args, "/w");
        let argv = argv_strings(&cmd);
        assert!(!argv.iter().any(|a| a.contains("=")));
    }

    #[test]
    fn compose_zero_args() {
        let cmd = compose_dispatch_command("notion-whoami", &HashMap::new(), "/w");
        assert_eq!(
            argv_strings(&cmd),
            vec!["/etc/toolset/dispatch", "notion-whoami"]
        );
    }
}
