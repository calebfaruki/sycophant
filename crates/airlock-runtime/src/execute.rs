use std::collections::HashMap;

use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Error, Debug)]
pub enum ExecuteError {
    #[error("command failed: {0}")]
    CommandFailed(#[from] std::io::Error),
}

pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    /// `Some(sig)` when the child was terminated by a signal (e.g. `9` for the
    /// cancel-driven SIGKILL) rather than exiting normally. Distinguishes a
    /// killed call from a normal exit that happens to be non-zero.
    pub terminated_by_signal: Option<i32>,
}

impl From<std::process::Output> for CommandResult {
    fn from(output: std::process::Output) -> Self {
        use std::os::unix::process::ExitStatusExt;
        Self {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
            terminated_by_signal: output.status.signal(),
        }
    }
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

/// Chamber-provided dispatcher path. Convention: every chamber image places
/// an executable here. Airlock spawns it with `argv = [tool_name]`, env =
/// arg values (one env var per declared arg, named per the schema), and
/// cwd = working_dir. The dispatcher decides how to route — Makefile, case
/// statement, Python script, native binary — entirely the chamber author's
/// choice. Not LLM-derived, never overridable per call.
pub const CHAMBER_DISPATCH: &str = "/etc/chamber/dispatch";

/// Build the dispatcher invocation for a tool call. argv is exactly
/// `[CHAMBER_DISPATCH, tool_name]`. Arg values flow in via environment
/// variables — never as `KEY=val` argv positions, which would let make-style
/// dispatchers smuggle the value into recipe text before the shell parses
/// it.
///
/// Pure: no spawning, no I/O. Returns a configured `tokio::process::Command`
/// for `run_dispatch` to spawn (or for tests to inspect).
pub fn compose_dispatch_command(
    tool_name: &str,
    args: &HashMap<String, String>,
    working_dir: &str,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(CHAMBER_DISPATCH);
    cmd.arg(tool_name);
    cmd.current_dir(working_dir);
    for (env_name, val) in args {
        cmd.env(env_name, val);
    }
    cmd
}

pub async fn run_dispatch(
    tool_name: &str,
    args: &HashMap<String, String>,
    working_dir: &str,
    cancel: &CancellationToken,
) -> Result<CommandResult, ExecuteError> {
    if crate::stdlib::BUILTIN_NAMES.contains(&tool_name) {
        return Ok(crate::stdlib::dispatch_builtin(
            tool_name,
            args,
            working_dir,
            crate::stdlib::DEFAULT_MAX_OUTPUT_CHARS,
            cancel,
        )
        .await);
    }
    let cmd = compose_dispatch_command(tool_name, args, working_dir);
    Ok(run_supervised_child(cmd, cancel, None).await?)
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
            vec!["/etc/chamber/dispatch", "notion-search"]
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
        // chamber dispatcher unchanged. The dispatcher (Makefile,
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
            vec!["/etc/chamber/dispatch", "notion-whoami"]
        );
    }

    #[tokio::test]
    async fn run_dispatch_routes_builtin_to_stdlib_not_chamber_dispatcher() {
        // Builtin names must take the in-process stdlib branch. If the
        // branch flipped to the chamber-dispatcher fallback, this test
        // would fail because /etc/chamber/dispatch does not exist on the
        // host — a `true` Shell invocation can only succeed via stdlib.
        let mut args = HashMap::new();
        args.insert("command".to_string(), "true".to_string());
        let cancel = CancellationToken::new();
        let result = run_dispatch("Shell", &args, "/tmp", &cancel)
            .await
            .expect("builtin branch must not surface ExecuteError");
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn run_dispatch_falls_through_to_chamber_dispatcher_for_non_builtin() {
        // Non-builtin names must NOT take the stdlib branch; they spawn
        // /etc/chamber/dispatch instead. On the test host that path is
        // absent, so the spawn surfaces an io::Error → ExecuteError.
        let cancel = CancellationToken::new();
        match run_dispatch("not-a-builtin", &HashMap::new(), "/tmp", &cancel).await {
            Err(ExecuteError::CommandFailed(io_err)) => {
                assert_eq!(io_err.kind(), std::io::ErrorKind::NotFound);
            }
            Ok(_) => panic!("non-builtin must attempt to spawn the chamber dispatcher, not stdlib"),
        }
    }

    // The trusted runtime retains a handle to the child it spawned on the
    // model's behalf and kills it when the turn's cancel arrives, rather than
    // let it run to completion; an uncancelled child runs to its normal exit.
    // This is the unit-provable causal chain up to the kill syscall — the real
    // kill across the pod boundary under the sandbox is proven by the e2e, not
    // unit-tested.

    // When the runtime is executing a chamber tool call and a cancel for that
    // call's identifier arrives, the runtime kills the child process it spawned
    // rather than allow it to run to completion, and the call's result reflects
    // a killed/signal termination rather than a normal exit.
    #[tokio::test]
    async fn runtime_kills_retained_child_on_cancel() {
        use std::time::{Duration, Instant};

        let mut args = HashMap::new();
        args.insert("command".to_string(), "sleep 30".to_string());

        let cancel = tokio_util::sync::CancellationToken::new();
        let fire = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            fire.cancel();
        });

        let start = Instant::now();
        let r = run_dispatch("Shell", &args, "/tmp", &cancel)
            .await
            .expect("shell builtin runs");
        let elapsed = start.elapsed();

        // Materiality: a mutant that ignores the cancel arm (or drops the
        // retained child without killing it) lets `sleep 30` run to natural
        // completion — elapsed climbs past 30s (this reds) and the child exits
        // normally rather than by signal (Some(9) below reds).
        assert!(
            elapsed < Duration::from_secs(5),
            "cancel must kill the retained child near cancel-time, not at the command's \
             natural 30s duration (elapsed {elapsed:?})"
        );
        assert_eq!(
            r.terminated_by_signal,
            Some(9),
            "the killed child must report SIGKILL termination"
        );
    }

    // A chamber tool call that runs to completion without any cancellation
    // returns its result unchanged.
    #[tokio::test]
    async fn uncancelled_child_runs_to_completion_with_normal_exit() {
        let mut args = HashMap::new();
        args.insert("command".to_string(), "exit 7".to_string());

        let cancel = tokio_util::sync::CancellationToken::new(); // never fired

        let r = run_dispatch("Shell", &args, "/tmp", &cancel)
            .await
            .expect("shell builtin runs");

        // Materiality: a mutant that always reports a killed/signal termination
        // reds `terminated_by_signal == None`; a mutant that loses the real exit
        // status reds `exit_code == 7`.
        assert_eq!(
            r.terminated_by_signal, None,
            "a normally-exiting child is not signal-terminated"
        );
        assert_eq!(r.exit_code, 7, "the child's real exit code is preserved");
    }
}
