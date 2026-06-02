use std::collections::HashMap;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ExecuteError {
    #[error("command failed: {0}")]
    CommandFailed(#[from] std::io::Error),
}

pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl From<std::process::Output> for CommandResult {
    fn from(output: std::process::Output) -> Self {
        Self {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        }
    }
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
) -> Result<CommandResult, ExecuteError> {
    if crate::stdlib::BUILTIN_NAMES.contains(&tool_name) {
        return Ok(crate::stdlib::dispatch_builtin(
            tool_name,
            args,
            working_dir,
            crate::stdlib::DEFAULT_MAX_OUTPUT_CHARS,
        )
        .await);
    }
    let mut cmd = compose_dispatch_command(tool_name, args, working_dir);
    let output = cmd.output().await?;
    Ok(output.into())
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
        // host — a `true` Bash invocation can only succeed via stdlib.
        let mut args = HashMap::new();
        args.insert("command".to_string(), "true".to_string());
        let result = run_dispatch("Bash", &args, "/tmp")
            .await
            .expect("builtin branch must not surface ExecuteError");
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn run_dispatch_falls_through_to_chamber_dispatcher_for_non_builtin() {
        // Non-builtin names must NOT take the stdlib branch; they spawn
        // /etc/chamber/dispatch instead. On the test host that path is
        // absent, so the spawn surfaces an io::Error → ExecuteError.
        match run_dispatch("not-a-builtin", &HashMap::new(), "/tmp").await {
            Err(ExecuteError::CommandFailed(io_err)) => {
                assert_eq!(io_err.kind(), std::io::ErrorKind::NotFound);
            }
            Ok(_) => panic!("non-builtin must attempt to spawn the chamber dispatcher, not stdlib"),
        }
    }
}
