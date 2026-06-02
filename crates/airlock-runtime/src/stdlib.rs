use std::collections::HashMap;

use crate::execute::CommandResult;

pub const BUILTIN_NAMES: &[&str] = &["Bash", "ReadFile", "WriteFile", "ListDirectory"];

pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 30_000;

pub async fn dispatch_builtin(
    name: &str,
    args: &HashMap<String, String>,
    working_dir: &str,
    max_output_chars: usize,
) -> CommandResult {
    let result = match name {
        "Bash" => execute_bash(args, working_dir).await,
        "ReadFile" => execute_read_file(args).await,
        "WriteFile" => execute_write_file(args).await,
        "ListDirectory" => execute_list_directory(args).await,
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

async fn execute_bash(args: &HashMap<String, String>, working_dir: &str) -> CommandResult {
    let command = match require(args, "command") {
        Ok(c) => c,
        Err(e) => return e,
    };

    match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .output()
        .await
    {
        Ok(output) => CommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        },
        Err(e) => error_result(format!("failed to execute command: {e}")),
    }
}

async fn execute_read_file(args: &HashMap<String, String>) -> CommandResult {
    let path = match require(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    match tokio::fs::read_to_string(path).await {
        Ok(content) => ok_result(content),
        Err(e) => error_result(format!("failed to read {path}: {e}")),
    }
}

async fn execute_write_file(args: &HashMap<String, String>) -> CommandResult {
    let path = match require(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let content = match require(args, "content") {
        Ok(c) => c,
        Err(e) => return e,
    };
    match tokio::fs::write(path, content).await {
        Ok(()) => ok_result(format!("wrote {} bytes to {path}", content.len())),
        Err(e) => error_result(format!("failed to write {path}: {e}")),
    }
}

async fn execute_list_directory(args: &HashMap<String, String>) -> CommandResult {
    let path = match require(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    match tokio::fs::read_dir(path).await {
        Ok(mut entries) => {
            let mut names = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
            names.sort();
            ok_result(names.join("\n"))
        }
        Err(e) => error_result(format!("failed to list {path}: {e}")),
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

    #[tokio::test]
    async fn bash_echo_succeeds() {
        let r = dispatch_builtin("Bash", &args(&[("command", "echo hello")]), "/tmp", 30_000).await;
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.contains("hello"));
        assert!(r.stderr.is_empty());
    }

    #[tokio::test]
    async fn bash_failing_command_marks_nonzero_exit() {
        let r = dispatch_builtin("Bash", &args(&[("command", "exit 42")]), "/tmp", 30_000).await;
        assert_eq!(r.exit_code, 42);
    }

    #[tokio::test]
    async fn bash_missing_command_param_is_error() {
        let r = dispatch_builtin("Bash", &args(&[]), "/tmp", 30_000).await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("missing required parameter"));
    }

    #[tokio::test]
    async fn bash_uses_working_dir_as_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let r = dispatch_builtin(
            "Bash",
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
    async fn read_file_returns_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, "file content").unwrap();
        let r = dispatch_builtin(
            "ReadFile",
            &args(&[("path", path.to_str().unwrap())]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.stdout, "file content");
    }

    #[tokio::test]
    async fn read_file_missing_path_param_is_error() {
        let r = dispatch_builtin("ReadFile", &args(&[]), "/tmp", 30_000).await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("missing required parameter"));
    }

    #[tokio::test]
    async fn read_file_missing_file_is_error() {
        let r = dispatch_builtin(
            "ReadFile",
            &args(&[("path", "/nonexistent/file.txt")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("failed to read"));
    }

    #[tokio::test]
    async fn write_file_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let r = dispatch_builtin(
            "WriteFile",
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
    async fn list_directory_sorts_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let r = dispatch_builtin(
            "ListDirectory",
            &args(&[("path", dir.path().to_str().unwrap())]),
            "/tmp",
            30_000,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.stdout.lines().collect::<Vec<_>>(), vec!["a.txt", "b.txt"]);
    }

    #[tokio::test]
    async fn list_directory_missing_dir_is_error() {
        let r = dispatch_builtin(
            "ListDirectory",
            &args(&[("path", "/nonexistent/dir")]),
            "/tmp",
            30_000,
        )
        .await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("failed to list"));
    }

    #[tokio::test]
    async fn unknown_builtin_is_error() {
        let r = dispatch_builtin("bogus", &args(&[]), "/tmp", 30_000).await;
        assert_ne!(r.exit_code, 0);
        assert!(r.stderr.contains("unknown builtin"));
    }

    #[tokio::test]
    async fn bash_output_truncated_at_limit() {
        let r = dispatch_builtin(
            "Bash",
            &args(&[("command", "yes | head -n 10000")]),
            "/tmp",
            200,
        )
        .await;
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.len() <= 200);
        assert!(r.stdout.contains("[...truncated"));
    }

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
        let marker_end = result.rfind(" characters...]\n").expect("marker end missing")
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
        let marker_end = result.rfind(" characters...]\n").expect("marker end missing")
            + " characters...]\n".len();
        let head_len = marker_start;
        let tail_len = result.len() - marker_end;
        let expected_truncated = 1000 - head_len - tail_len;
        assert!(result.contains(&format!(
            "[...truncated {expected_truncated} characters...]"
        )));
    }

    #[tokio::test]
    async fn bash_signal_killed_process_reports_negative_one() {
        let r = dispatch_builtin("Bash", &args(&[("command", "kill -9 $$")]), "/tmp", 30_000).await;
        assert_eq!(r.exit_code, -1);
    }
}
