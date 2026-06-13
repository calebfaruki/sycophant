use std::collections::HashMap;

use crate::execute::CommandResult;

pub const BUILTIN_NAMES: &[&str] = &["Shell", "Read", "Write", "Edit", "Search"];

pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 30_000;

const DEFAULT_SHELL_TIMEOUT_SECS: u64 = 60;
const MAX_SHELL_TIMEOUT_SECS: u64 = 600;
const READ_FILE_CAP_BYTES: u64 = 1_048_576;
const SEARCH_MAX_PAGE: usize = 1000;
const SEARCH_DEFAULT_LIMIT: usize = 50;
const SEARCH_MAX_COUNT_PER_FILE: &str = "100";

pub async fn dispatch_builtin(
    name: &str,
    args: &HashMap<String, String>,
    working_dir: &str,
    max_output_chars: usize,
) -> CommandResult {
    let result = match name {
        "Shell" => execute_shell(args, working_dir).await,
        "Read" => execute_read(args).await,
        "Write" => execute_write(args).await,
        "Edit" => execute_edit(args).await,
        "Search" => execute_search(args, working_dir).await,
        _ => error_result(format!("unknown builtin: {name}")),
    };
    CommandResult {
        stdout: truncate_middle(&result.stdout, max_output_chars),
        stderr: truncate_middle(&result.stderr, max_output_chars),
        exit_code: result.exit_code,
    }
}

fn ok_result(stdout: String) -> CommandResult {
    CommandResult {
        stdout,
        stderr: String::new(),
        exit_code: 0,
    }
}

fn error_result(stderr: String) -> CommandResult {
    CommandResult {
        stdout: String::new(),
        stderr,
        exit_code: 1,
    }
}

fn require<'a>(args: &'a HashMap<String, String>, key: &str) -> Result<&'a str, CommandResult> {
    args.get(key)
        .map(String::as_str)
        .ok_or_else(|| error_result(format!("missing required parameter: {key}")))
}

fn parse_optional_u64(args: &HashMap<String, String>, key: &str, default: u64) -> u64 {
    args.get(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_optional_usize(args: &HashMap<String, String>, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Reject files larger than `READ_FILE_CAP_BYTES` before we attempt to slurp
/// them into memory. Shared by `Read` and `Edit` so both hold the same OOM
/// boundary.
async fn check_size_cap(path: &str) -> Result<(), CommandResult> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => {
            if meta.len() > READ_FILE_CAP_BYTES {
                Err(error_result(format!(
                    "file too large: {} bytes exceeds {} byte cap",
                    meta.len(),
                    READ_FILE_CAP_BYTES
                )))
            } else {
                Ok(())
            }
        }
        Err(e) => Err(error_result(format!("failed to read {path}: {e}"))),
    }
}

async fn execute_shell(args: &HashMap<String, String>, working_dir: &str) -> CommandResult {
    let command = match require(args, "command") {
        Ok(c) => c,
        Err(e) => return e,
    };
    let timeout_secs =
        parse_optional_u64(args, "timeout", DEFAULT_SHELL_TIMEOUT_SECS).min(MAX_SHELL_TIMEOUT_SECS);
    let workdir = args
        .get("workdir")
        .map(String::as_str)
        .unwrap_or(working_dir);

    let fut = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(workdir)
        .output();

    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), fut).await {
        Ok(Ok(output)) => CommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        },
        Ok(Err(e)) => error_result(format!("failed to execute command: {e}")),
        Err(_) => CommandResult {
            stdout: String::new(),
            stderr: format!("command timed out after {timeout_secs}s"),
            exit_code: -1,
        },
    }
}

async fn execute_read(args: &HashMap<String, String>) -> CommandResult {
    let path = match require(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let offset = parse_optional_usize(args, "offset", 1).max(1);
    let limit = parse_optional_usize(args, "limit", usize::MAX);

    if let Err(e) = check_size_cap(path).await {
        return e;
    }

    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            let mut out = String::new();
            let mut included = 0usize;
            for (idx, line) in content.lines().enumerate() {
                let lineno = idx + 1;
                if lineno < offset {
                    continue;
                }
                if included >= limit {
                    break;
                }
                out.push_str(&format!("{lineno}|{line}\n"));
                included += 1;
            }
            ok_result(out)
        }
        Err(e) => error_result(format!("failed to read {path}: {e}")),
    }
}

async fn execute_write(args: &HashMap<String, String>) -> CommandResult {
    let path = match require(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let content = match require(args, "content") {
        Ok(c) => c,
        Err(e) => return e,
    };

    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return error_result(format!("failed to create parents for {path}: {e}"));
            }
        }
    }

    match tokio::fs::write(path, content).await {
        Ok(()) => ok_result(format!("wrote {} bytes to {path}", content.len())),
        Err(e) => error_result(format!("failed to write {path}: {e}")),
    }
}

async fn execute_edit(args: &HashMap<String, String>) -> CommandResult {
    let path = match require(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let old_string = match require(args, "old_string") {
        Ok(s) => s,
        Err(e) => return e,
    };
    let new_string = match require(args, "new_string") {
        Ok(s) => s,
        Err(e) => return e,
    };
    if old_string == new_string {
        return error_result("old_string and new_string are identical".to_string());
    }

    if let Err(e) = check_size_cap(path).await {
        return e;
    }

    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) => return error_result(format!("failed to read {path}: {e}")),
    };

    let match_count = content.matches(old_string).count();
    if match_count == 0 {
        return error_result(format!("old_string not found in {path}"));
    }
    if match_count > 1 {
        return error_result(format!(
            "old_string matches {match_count} locations in {path}; must be unique"
        ));
    }

    let updated = content.replacen(old_string, new_string, 1);
    match tokio::fs::write(path, &updated).await {
        Ok(()) => ok_result(format!("edited {path}")),
        Err(e) => error_result(format!("failed to write {path}: {e}")),
    }
}

/// Local discriminant for the `Search` tool's two modes. Kept private to
/// `execute_search` so the public arg surface stays string-based for the
/// chamber dispatch protocol.
#[derive(Clone, Copy)]
enum SearchTarget {
    Content,
    Files,
}

async fn execute_search(args: &HashMap<String, String>, working_dir: &str) -> CommandResult {
    let target_raw = match require(args, "target") {
        Ok(t) => t,
        Err(e) => return e,
    };
    let target = match target_raw {
        "content" => SearchTarget::Content,
        "files" => SearchTarget::Files,
        other => {
            return error_result(format!(
                "target must be 'content' or 'files', got '{other}'"
            ));
        }
    };
    let pattern = match require(args, "pattern") {
        Ok(p) => p,
        Err(e) => return e,
    };
    if matches!(target, SearchTarget::Content) && pattern.is_empty() {
        return error_result("pattern cannot be empty for content search".to_string());
    }
    let path = args.get("path").map(String::as_str).unwrap_or(working_dir);
    let glob = args.get("glob").map(String::as_str);
    let limit = parse_optional_usize(args, "limit", SEARCH_DEFAULT_LIMIT);
    let offset = parse_optional_usize(args, "offset", 0);

    if offset.saturating_add(limit) > SEARCH_MAX_PAGE {
        return error_result(format!(
            "offset + limit ({}) exceeds {SEARCH_MAX_PAGE} cap",
            offset.saturating_add(limit)
        ));
    }

    let mut cmd = tokio::process::Command::new("rg");
    cmd.arg("--no-follow").arg("--no-ignore");
    if let Some(g) = glob {
        cmd.arg("--glob").arg(g);
    }

    // `--` ends ripgrep's flag parsing so an LLM-supplied pattern or path
    // beginning with `-` (e.g. `-A50`, `--type-add=…`) is treated as a
    // positional argument, not a flag.
    match target {
        SearchTarget::Files => {
            cmd.arg("--files").arg("--").arg(path);
        }
        SearchTarget::Content => {
            cmd.arg("--max-count").arg(SEARCH_MAX_COUNT_PER_FILE);
            cmd.arg("--").arg(pattern).arg(path);
        }
    }

    match cmd.output().await {
        Ok(output) => {
            let exit = output.status.code().unwrap_or(-1);
            // ripgrep exit 1 = "no matches" (content) or "no files" (--files);
            // exit 2+ = real error. Both modes treat 1 as success-empty.
            if exit == 1 {
                return ok_result(String::new());
            }
            if exit != 0 {
                return CommandResult {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    exit_code: exit,
                };
            }

            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let mut lines: Vec<&str> = stdout_str.lines().collect();

            if matches!(target, SearchTarget::Files) && !pattern.is_empty() {
                let needle = pattern.to_lowercase();
                lines.retain(|p| {
                    std::path::Path::new(p)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_lowercase().contains(&needle))
                        .unwrap_or(false)
                });
            }

            let paginated: Vec<&str> = lines.into_iter().skip(offset).take(limit).collect();
            ok_result(paginated.join("\n"))
        }
        Err(e) => error_result(format!("failed to invoke rg: {e}")),
    }
}

fn truncate_middle(output: &str, max_chars: usize) -> String {
    if output.len() <= max_chars {
        return output.to_string();
    }
    let marker_template = "\n[...truncated 0 characters...]\n";
    let digit_count = (output.len() - max_chars).to_string().len();
    let marker_len = marker_template.len() - 1 + digit_count;
    if max_chars <= marker_len {
        return output[..max_chars].to_string();
    }
    let available = max_chars - marker_len;
    let head_len = available * 2 / 5;
    let tail_len = available - head_len;
    let head = &output[..head_len];
    let tail = &output[output.len() - tail_len..];
    let truncated = output.len() - head_len - tail_len;
    format!("{head}\n[...truncated {truncated} characters...]\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // ---- Shell ----

    #[tokio::test]
    async fn shell_echo_succeeds() {
        let r =
            dispatch_builtin("Shell", &args(&[("command", "echo hello")]), "/tmp", 30_000).await;
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.contains("hello"));
        assert!(r.stderr.is_empty());
    }

    #[tokio::test]
    async fn shell_failing_command_marks_nonzero_exit() {
        let r = dispatch_builtin("Shell", &args(&[("command", "exit 42")]), "/tmp", 30_000).await;
        assert_eq!(r.exit_code, 42);
    }

    #[tokio::test]
    async fn shell_missing_command_param_is_error() {
        let r = dispatch_builtin("Shell", &args(&[]), "/tmp", 30_000).await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("missing required parameter"));
    }

    #[tokio::test]
    async fn shell_uses_working_dir_as_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let r = dispatch_builtin(
            "Shell",
            &args(&[("command", "pwd")]),
            dir.path().to_str().unwrap(),
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        let actual = std::fs::canonicalize(r.stdout.trim()).unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn shell_workdir_param_overrides_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let r = dispatch_builtin(
            "Shell",
            &args(&[
                ("command", "pwd"),
                ("workdir", dir.path().to_str().unwrap()),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        let actual = std::fs::canonicalize(r.stdout.trim()).unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn shell_output_truncated_at_limit() {
        let r = dispatch_builtin(
            "Shell",
            &args(&[("command", "yes | head -n 10000")]),
            "/tmp",
            200,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.len() <= 200);
        assert!(r.stdout.contains("[...truncated"));
    }

    #[tokio::test]
    async fn shell_signal_killed_process_reports_negative_one() {
        let r =
            dispatch_builtin("Shell", &args(&[("command", "kill -9 $$")]), "/tmp", 30_000).await;
        assert_eq!(r.exit_code, -1);
    }

    #[tokio::test]
    async fn shell_timeout_kills_long_running_command() {
        let r = dispatch_builtin(
            "Shell",
            &args(&[("command", "sleep 10"), ("timeout", "1")]),
            "/tmp",
            30_000,
        )
        .await;
        // Timeout returns the same -1 sentinel as a signal-killed process so
        // downstream callers route both as "abnormal exit" without branching
        // on positive vs negative codes.
        assert_eq!(r.exit_code, -1);
        assert!(r.stderr.contains("timed out"));
    }

    #[tokio::test]
    async fn shell_timeout_clamped_to_max() {
        // Requesting a timeout above the hard cap is silently clamped — the
        // command still runs to completion within the cap. Hard to test the
        // clamp directly without making the cap configurable, so just verify
        // a "too-large" request doesn't reject and a quick command completes.
        let r = dispatch_builtin(
            "Shell",
            &args(&[("command", "echo ok"), ("timeout", "99999")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.contains("ok"));
    }

    // ---- Read ----

    #[tokio::test]
    async fn read_line_numbers_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let r = dispatch_builtin(
            "Read",
            &args(&[("path", path.to_str().unwrap())]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.stdout, "1|alpha\n2|beta\n3|gamma\n");
    }

    #[tokio::test]
    async fn read_offset_skips_initial_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, "a\nb\nc\nd\n").unwrap();
        let r = dispatch_builtin(
            "Read",
            &args(&[("path", path.to_str().unwrap()), ("offset", "3")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.stdout, "3|c\n4|d\n");
    }

    #[tokio::test]
    async fn read_limit_caps_returned_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();
        let r = dispatch_builtin(
            "Read",
            &args(&[("path", path.to_str().unwrap()), ("limit", "2")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.stdout, "1|a\n2|b\n");
    }

    #[tokio::test]
    async fn read_offset_plus_limit_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();
        let r = dispatch_builtin(
            "Read",
            &args(&[
                ("path", path.to_str().unwrap()),
                ("offset", "2"),
                ("limit", "2"),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.stdout, "2|b\n3|c\n");
    }

    #[tokio::test]
    async fn read_missing_path_param_is_error() {
        let r = dispatch_builtin("Read", &args(&[]), "/tmp", 30_000).await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("missing required parameter"));
    }

    #[tokio::test]
    async fn read_missing_file_is_error() {
        let r = dispatch_builtin(
            "Read",
            &args(&[("path", "/nonexistent/file.txt")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("failed to read"));
    }

    #[tokio::test]
    async fn read_oversized_file_is_error_no_partial_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let oversized = "x".repeat(READ_FILE_CAP_BYTES as usize + 1);
        std::fs::write(&path, &oversized).unwrap();
        let r = dispatch_builtin(
            "Read",
            &args(&[("path", path.to_str().unwrap())]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("too large"));
        assert!(r.stdout.is_empty());
    }

    #[tokio::test]
    async fn read_at_exact_cap_succeeds() {
        // Pins the cap as strict `>` rather than `>=`. A file of exactly
        // READ_FILE_CAP_BYTES is still permitted.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("at_cap.txt");
        let at_cap = "x".repeat(READ_FILE_CAP_BYTES as usize);
        std::fs::write(&path, &at_cap).unwrap();
        let r = dispatch_builtin(
            "Read",
            &args(&[("path", path.to_str().unwrap())]),
            "/tmp",
            usize::MAX,
        )
        .await;
        assert_eq!(r.exit_code, 0);
    }

    // ---- Write ----

    #[tokio::test]
    async fn write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let r = dispatch_builtin(
            "Write",
            &args(&[("path", path.to_str().unwrap()), ("content", "hello")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.contains("wrote 5 bytes"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[tokio::test]
    async fn write_auto_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/out.txt");
        let r = dispatch_builtin(
            "Write",
            &args(&[("path", path.to_str().unwrap()), ("content", "nested")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "nested");
    }

    #[tokio::test]
    async fn write_empty_content_creates_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        let r = dispatch_builtin(
            "Write",
            &args(&[("path", path.to_str().unwrap()), ("content", "")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    // ---- Edit ----

    #[tokio::test]
    async fn edit_replaces_unique_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, "the quick brown fox").unwrap();
        let r = dispatch_builtin(
            "Edit",
            &args(&[
                ("path", path.to_str().unwrap()),
                ("old_string", "quick"),
                ("new_string", "lazy"),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "the lazy brown fox"
        );
    }

    #[tokio::test]
    async fn edit_missing_anchor_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, "hello world").unwrap();
        let r = dispatch_builtin(
            "Edit",
            &args(&[
                ("path", path.to_str().unwrap()),
                ("old_string", "xyz"),
                ("new_string", "abc"),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("not found"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn edit_ambiguous_anchor_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, "foo bar foo").unwrap();
        let r = dispatch_builtin(
            "Edit",
            &args(&[
                ("path", path.to_str().unwrap()),
                ("old_string", "foo"),
                ("new_string", "baz"),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("matches 2"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo bar foo");
    }

    #[tokio::test]
    async fn edit_identical_old_and_new_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, "noop").unwrap();
        let r = dispatch_builtin(
            "Edit",
            &args(&[
                ("path", path.to_str().unwrap()),
                ("old_string", "noop"),
                ("new_string", "noop"),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("identical"));
    }

    #[tokio::test]
    async fn edit_runaway_protected_when_new_contains_old() {
        // Old="foo", new="foobar" — replacen with limit 1 prevents
        // re-matching the inserted "foo" inside "foobar".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, "foo").unwrap();
        let r = dispatch_builtin(
            "Edit",
            &args(&[
                ("path", path.to_str().unwrap()),
                ("old_string", "foo"),
                ("new_string", "foobar"),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "foobar");
    }

    #[tokio::test]
    async fn edit_missing_required_param_is_error() {
        let r = dispatch_builtin(
            "Edit",
            &args(&[("path", "/tmp/x"), ("old_string", "a")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("missing required parameter"));
    }

    #[tokio::test]
    async fn edit_oversized_file_is_error_no_read() {
        // Pins the file-size cap. Mirrors Read's cap so an LLM pointed
        // at a multi-MB file cannot OOM the runtime via Edit.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let oversized = "x".repeat(READ_FILE_CAP_BYTES as usize + 1);
        std::fs::write(&path, &oversized).unwrap();
        let r = dispatch_builtin(
            "Edit",
            &args(&[
                ("path", path.to_str().unwrap()),
                ("old_string", "x"),
                ("new_string", "y"),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("too large"));
        // File unchanged.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), oversized);
    }

    #[tokio::test]
    async fn edit_at_exact_cap_succeeds() {
        // Pins the cap as strict `>` rather than `>=`. A file of exactly
        // READ_FILE_CAP_BYTES is still editable.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("at_cap.txt");
        let mut content = String::with_capacity(READ_FILE_CAP_BYTES as usize);
        content.push_str("ANCHOR");
        content.push_str(&"x".repeat(READ_FILE_CAP_BYTES as usize - "ANCHOR".len()));
        assert_eq!(content.len() as u64, READ_FILE_CAP_BYTES);
        std::fs::write(&path, &content).unwrap();
        let r = dispatch_builtin(
            "Edit",
            &args(&[
                ("path", path.to_str().unwrap()),
                ("old_string", "ANCHOR"),
                ("new_string", "MARKER"),
            ]),
            "/tmp",
            usize::MAX,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .starts_with("MARKER"));
    }

    // ---- Search ----

    #[tokio::test]
    async fn search_files_empty_pattern_lists_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        let r = dispatch_builtin(
            "Search",
            &args(&[
                ("target", "files"),
                ("pattern", ""),
                ("path", dir.path().to_str().unwrap()),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        let mut names: Vec<&str> = r
            .stdout
            .lines()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
            })
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.txt", "b.txt"]);
    }

    #[tokio::test]
    async fn search_files_basename_substring_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        std::fs::write(dir.path().join("readme.txt"), "").unwrap();
        std::fs::write(dir.path().join("other.rs"), "").unwrap();
        let r = dispatch_builtin(
            "Search",
            &args(&[
                ("target", "files"),
                ("pattern", "readme"),
                ("path", dir.path().to_str().unwrap()),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        let mut names: Vec<&str> = r
            .stdout
            .lines()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
            })
            .collect();
        names.sort();
        assert_eq!(names, vec!["README.md", "readme.txt"]);
    }

    #[tokio::test]
    async fn search_content_finds_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let r = dispatch_builtin(
            "Search",
            &args(&[
                ("target", "content"),
                ("pattern", "beta"),
                ("path", dir.path().to_str().unwrap()),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.contains("beta"));
        assert!(r.stdout.contains("a.txt"));
    }

    #[tokio::test]
    async fn search_content_post_filter_does_not_apply_to_content_mode() {
        // Discriminates the `&&` guard on the post-filter. With `||`, the
        // filter would run in content mode and strip lines whose basename
        // (including the `:line:content` suffix ripgrep emits) doesn't
        // contain the literal pattern. A regex like `b.t` matching `bat`
        // is a clean discriminator: ripgrep returns the match, but the
        // literal substring `b.t` is absent from `<file>:1:bat`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "bat").unwrap();
        let r = dispatch_builtin(
            "Search",
            &args(&[
                ("target", "content"),
                ("pattern", "b.t"),
                ("path", dir.path().to_str().unwrap()),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.contains("bat"));
    }

    #[tokio::test]
    async fn search_content_no_matches_is_empty_success() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let r = dispatch_builtin(
            "Search",
            &args(&[
                ("target", "content"),
                ("pattern", "nonexistent_string_xyz_42"),
                ("path", dir.path().to_str().unwrap()),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.is_empty());
    }

    #[tokio::test]
    async fn search_invalid_target_is_error() {
        let r = dispatch_builtin(
            "Search",
            &args(&[("target", "bogus"), ("pattern", "x")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("target must be"));
    }

    #[tokio::test]
    async fn search_pagination_cap_enforced() {
        let r = dispatch_builtin(
            "Search",
            &args(&[
                ("target", "files"),
                ("pattern", ""),
                ("path", "/tmp"),
                ("offset", "500"),
                ("limit", "600"),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("exceeds"));
    }

    #[tokio::test]
    async fn search_pagination_at_exact_cap_allowed() {
        // Pins the cap as strict `>` rather than `>=`. offset + limit
        // exactly at SEARCH_MAX_PAGE is permitted.
        let dir = tempfile::tempdir().unwrap();
        let r = dispatch_builtin(
            "Search",
            &args(&[
                ("target", "files"),
                ("pattern", ""),
                ("path", dir.path().to_str().unwrap()),
                ("offset", "500"),
                ("limit", "500"),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
    }

    #[tokio::test]
    async fn search_files_pagination_skips_and_takes() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("{i:02}.txt")), "").unwrap();
        }
        let r = dispatch_builtin(
            "Search",
            &args(&[
                ("target", "files"),
                ("pattern", ""),
                ("path", dir.path().to_str().unwrap()),
                ("offset", "3"),
                ("limit", "2"),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        let line_count = r.stdout.lines().count();
        assert_eq!(line_count, 2, "limit should cap returned lines");
    }

    #[tokio::test]
    async fn search_content_empty_pattern_is_error() {
        // Content mode with empty pattern would otherwise pass `""` to
        // ripgrep and match every line of every file. Symmetric guard to
        // the files-mode "empty lists all" semantic.
        let r = dispatch_builtin(
            "Search",
            &args(&[("target", "content"), ("pattern", "")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("pattern cannot be empty"));
    }

    #[tokio::test]
    async fn search_content_pattern_starting_with_dash_treated_as_literal() {
        // Without the `--` end-of-options separator, ripgrep parses
        // `--help` as a flag and dumps usage to stdout. With `--`, the
        // pattern is matched literally as a regex.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t.txt"), "use --help to see usage").unwrap();
        let r = dispatch_builtin(
            "Search",
            &args(&[
                ("target", "content"),
                ("pattern", "--help"),
                ("path", dir.path().to_str().unwrap()),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        // Help text begins with "Usage:" / "USAGE:"; flag-parse would emit it.
        assert!(!r.stdout.to_lowercase().contains("usage:"));
        assert!(r.stdout.contains("--help"));
    }

    #[tokio::test]
    async fn search_files_path_starting_with_dash_not_interpreted_as_flag() {
        // A directory whose name starts with `-` would be parsed as a
        // flag without the `--` separator.
        let parent = tempfile::tempdir().unwrap();
        let dash_dir = parent.path().join("-A50");
        std::fs::create_dir(&dash_dir).unwrap();
        std::fs::write(dash_dir.join("inside.txt"), "").unwrap();
        let r = dispatch_builtin(
            "Search",
            &args(&[
                ("target", "files"),
                ("pattern", ""),
                ("path", dash_dir.to_str().unwrap()),
            ]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.contains("inside.txt"));
    }

    // ---- Dispatch ----

    #[tokio::test]
    async fn unknown_builtin_is_error() {
        let r = dispatch_builtin("bogus", &args(&[]), "/tmp", 30_000).await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("unknown builtin"));
    }

    // ---- truncate_middle ----

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate_middle("hello", 100), "hello");
    }

    #[test]
    fn truncate_exact_limit_unchanged() {
        assert_eq!(truncate_middle("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string_has_marker_with_count() {
        let s = "a".repeat(1000);
        let result = truncate_middle(&s, 100);
        assert_eq!(result.len(), 100);
        let marker_start = result.find("[...truncated ").expect("marker missing");
        let after_prefix = &result[marker_start + "[...truncated ".len()..];
        let count_end = after_prefix.find(' ').expect("marker malformed");
        let count: usize = after_prefix[..count_end].parse().expect("count not int");
        assert!(count > 0);
        assert!(result.contains(&format!("[...truncated {count} characters...]")));
    }

    #[test]
    fn truncate_preserves_head_and_tail() {
        let head_region = "H".repeat(50);
        let tail_region = "T".repeat(50);
        let s = format!("{}{}{}", head_region, "x".repeat(900), tail_region);
        let result = truncate_middle(&s, 100);
        assert!(result.starts_with("HH"));
        assert!(result.ends_with("TT"));
    }

    #[test]
    fn truncate_head_is_40_percent_of_available_space() {
        let s = "a".repeat(1000);
        let result = truncate_middle(&s, 100);
        let marker_start = result.find("\n[...truncated ").expect("marker missing");
        let marker_end = result
            .rfind(" characters...]\n")
            .expect("marker end missing")
            + " characters...]\n".len();
        let head_len = marker_start;
        let tail_len = result.len() - marker_end;
        let marker_len = marker_end - marker_start;
        let available = 100 - marker_len;
        let expected_head = available * 2 / 5;
        let expected_tail = available - expected_head;
        assert_eq!(head_len, expected_head, "head is 2/5 of available space");
        assert_eq!(tail_len, expected_tail, "tail is remainder of available");
    }

    #[test]
    fn truncate_marker_reports_exact_dropped_byte_count() {
        let s = "a".repeat(1000);
        let result = truncate_middle(&s, 100);
        let marker_start = result.find("\n[...truncated ").expect("marker missing");
        let marker_end = result
            .rfind(" characters...]\n")
            .expect("marker end missing")
            + " characters...]\n".len();
        let head_len = marker_start;
        let tail_len = result.len() - marker_end;
        let expected_truncated = 1000 - head_len - tail_len;
        assert!(result.contains(&format!(
            "[...truncated {expected_truncated} characters...]"
        )));
    }
}
