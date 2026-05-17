//! Chamber-build linter. Reads a chamber directory's Dockerfile, extracts
//! the LABEL's declared env-var names, and statically analyzes the dispatch
//! and Makefile files for shell-injection patterns that would let LLM-
//! controlled arg values escape the `"$VAR"` single-token boundary.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::cli::{ChamberCmd, ChamberSub};

pub(crate) fn run(cmd: ChamberCmd) -> Result<(), String> {
    match cmd.sub {
        ChamberSub::Lint(c) => lint(&c.path),
    }
}

fn lint(dir_str: &str) -> Result<(), String> {
    let dir = Path::new(dir_str);
    if !dir.is_dir() {
        return Err(format!("not a directory: {dir_str}"));
    }

    let dockerfile_path = dir.join("Dockerfile");
    let dockerfile_content = fs::read_to_string(&dockerfile_path)
        .map_err(|e| format!("failed to read {}: {e}", dockerfile_path.display()))?;
    let env_vars = extract_env_vars(&dockerfile_content)?;

    let mut diagnostics = Vec::new();
    if let Some(dispatch) = read_optional(&dir.join("dispatch"))? {
        diagnostics.extend(lint_shell(&dispatch, "dispatch", &env_vars));
    }
    if let Some(makefile) = read_optional(&dir.join("Makefile"))? {
        diagnostics.extend(lint_makefile(&makefile, "Makefile", &env_vars));
    }

    if diagnostics.is_empty() {
        eprintln!(
            "{}: OK ({} schema vars, no shell-injection patterns)",
            dir_str,
            env_vars.len()
        );
        Ok(())
    } else {
        for d in &diagnostics {
            eprintln!("{d}");
        }
        Err(format!("{} violations", diagnostics.len()))
    }
}

fn read_optional(p: &Path) -> Result<Option<String>, String> {
    if !p.exists() {
        return Ok(None);
    }
    fs::read_to_string(p)
        .map(Some)
        .map_err(|e| format!("failed to read {}: {e}", p.display()))
}

/// Parse the Dockerfile's `LABEL md.sycophant.tools='[...]'` value and collect
/// every declared env-var name (`args.<key>.env` across all tools).
pub(crate) fn extract_env_vars(dockerfile: &str) -> Result<HashSet<String>, String> {
    let collapsed = dockerfile.replace("\\\n", "");
    let label_pattern = "LABEL md.sycophant.tools=";
    let label_start = collapsed
        .find(label_pattern)
        .ok_or("Dockerfile missing `LABEL md.sycophant.tools=`")?;
    let after = &collapsed[label_start + label_pattern.len()..];
    let trimmed = after.trim_start();
    let body = trimmed
        .strip_prefix('\'')
        .ok_or("LABEL value must be single-quoted JSON")?;
    // Find the closing `'` at the end of the LABEL command (same logical
    // line after continuation collapse). Use rfind on the slice up to the
    // next newline so apostrophes inside description strings (e.g.
    // "integration's") don't truncate the value early.
    let line_end = body.find('\n').unwrap_or(body.len());
    let end = body[..line_end]
        .rfind('\'')
        .ok_or("unterminated LABEL value (no closing single quote on the LABEL line)")?;
    let json_str = &body[..end];

    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("LABEL JSON parse failed: {e}"))?;
    let array = parsed
        .as_array()
        .ok_or("LABEL value must be a JSON array")?;

    let mut env_vars = HashSet::new();
    for tool in array {
        if let Some(args) = tool.get("args").and_then(|a| a.as_object()) {
            for arg in args.values() {
                if let Some(env) = arg.get("env").and_then(|e| e.as_str()) {
                    env_vars.insert(env.to_string());
                }
            }
        }
    }

    Ok(env_vars)
}

#[derive(Debug)]
pub(crate) struct Diagnostic {
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}

/// Lint a shell script (e.g., the chamber `dispatch`). Flags:
/// - unquoted `$VAR` / `${VAR}` for any var in `env_vars`
/// - `$(...)` or backtick command substitution containing a schema var
/// - `eval` keyword (forbidden regardless of vars present)
pub(crate) fn lint_shell(content: &str, file: &str, env_vars: &HashSet<String>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line_no = i + 1;
        if line.trim_start().starts_with('#') {
            continue;
        }

        for var_ref in find_var_refs(line) {
            if env_vars.contains(&var_ref.name) && !var_ref.in_double_quotes {
                out.push(Diagnostic {
                    file: file.to_string(),
                    line: line_no,
                    message: format!(
                        "unquoted ${} (schema var must be in double quotes: \"${}\")",
                        var_ref.name, var_ref.name
                    ),
                });
            }
        }

        if let Some(kind) = command_subst_with_var(line, env_vars) {
            out.push(Diagnostic {
                file: file.to_string(),
                line: line_no,
                message: format!(
                    "schema var inside {kind}; tainted value would be re-parsed as shell"
                ),
            });
        }

        if has_eval(line) {
            out.push(Diagnostic {
                file: file.to_string(),
                line: line_no,
                message: "use of `eval` is forbidden in chamber dispatchers".to_string(),
            });
        }
    }
    out
}

/// Lint a Makefile. Recipe lines (tab-indented) are treated as make text
/// where `$(VAR)` is make-side expansion (must not be a schema var) and
/// `$$VAR` becomes shell `$VAR` (must be inside double quotes when a schema
/// var). Non-recipe lines are ignored.
pub(crate) fn lint_makefile(
    content: &str,
    file: &str,
    env_vars: &HashSet<String>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line_no = i + 1;
        if line.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with('\t') {
            continue;
        }

        for var in find_make_var_refs(line) {
            if env_vars.contains(&var) {
                out.push(Diagnostic {
                    file: file.to_string(),
                    line: line_no,
                    message: format!(
                        "make-side expansion $({var}) of schema var; use $${var} (escapes to shell $var) and quote it as \"$${var}\""
                    ),
                });
            }
        }

        let shell_form = line.replace("$$", "$");
        for var_ref in find_var_refs(&shell_form) {
            if env_vars.contains(&var_ref.name) && !var_ref.in_double_quotes {
                out.push(Diagnostic {
                    file: file.to_string(),
                    line: line_no,
                    message: format!(
                        "unquoted $${} in recipe (use \"$${}\")",
                        var_ref.name, var_ref.name
                    ),
                });
            }
        }

        if let Some(kind) = command_subst_with_var(&shell_form, env_vars) {
            out.push(Diagnostic {
                file: file.to_string(),
                line: line_no,
                message: format!(
                    "schema var inside {kind} in recipe; tainted value would be re-parsed as shell"
                ),
            });
        }

        if has_eval(line) {
            out.push(Diagnostic {
                file: file.to_string(),
                line: line_no,
                message: "use of `eval` is forbidden in chamber Makefiles".to_string(),
            });
        }
    }
    out
}

#[derive(Debug, PartialEq)]
struct VarRef {
    name: String,
    in_double_quotes: bool,
}

/// Scan shell text for `$VAR` and `${VAR}` patterns, tracking whether each
/// match is inside double quotes. Simple even/odd quote-count tracking; does
/// not handle escaped quotes or single-quote contexts because chamber
/// dispatchers should keep recipes simple.
fn find_var_refs(line: &str) -> Vec<VarRef> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut in_quote = false;
    while i < chars.len() {
        if chars[i] == '"' {
            in_quote = !in_quote;
            i += 1;
            continue;
        }
        if chars[i] == '$' && i + 1 < chars.len() {
            if chars[i + 1] == '{' {
                let start = i + 2;
                let mut end = start;
                while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                if end > start {
                    out.push(VarRef {
                        name: chars[start..end].iter().collect(),
                        in_double_quotes: in_quote,
                    });
                }
                while end < chars.len() && chars[end] != '}' {
                    end += 1;
                }
                i = if end < chars.len() { end + 1 } else { end };
                continue;
            }
            if chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '_' {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                out.push(VarRef {
                    name: chars[start..end].iter().collect(),
                    in_double_quotes: in_quote,
                });
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Scan a Makefile recipe line for `$(VAR)` patterns where VAR is a simple
/// identifier (alphanumeric + underscore). Skips `$$(...)` (which is shell
/// command substitution after make's `$$` → `$` escape). Skips `$(call ...)`,
/// `$(shell ...)`, etc. — those are functions, not bare-var expansion.
fn find_make_var_refs(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == '$' && chars[i + 1] == '(' {
            // $$( is shell, not make
            if i > 0 && chars[i - 1] == '$' {
                i += 2;
                continue;
            }
            let start = i + 2;
            let mut end = start;
            while end < chars.len() && chars[end] != ')' {
                end += 1;
            }
            if end > start {
                let inner: String = chars[start..end].iter().collect();
                if inner.chars().all(|c| c.is_alphanumeric() || c == '_') && !inner.is_empty() {
                    out.push(inner);
                }
            }
            i = end.saturating_add(1);
            continue;
        }
        i += 1;
    }
    out
}

/// If the line contains `$(...)` or backtick-bounded command substitution
/// that references any var in `env_vars`, return the kind string for the
/// diagnostic. Otherwise `None`.
fn command_subst_with_var(line: &str, env_vars: &HashSet<String>) -> Option<&'static str> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '$' && chars[i + 1] == '(' {
            let mut depth = 1;
            let mut j = i + 2;
            let start = j;
            while j < chars.len() && depth > 0 {
                if chars[j] == '(' {
                    depth += 1;
                } else if chars[j] == ')' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                j += 1;
            }
            let inner: String = chars[start..j].iter().collect();
            for var in env_vars {
                if inner.contains(&format!("${var}")) || inner.contains(&format!("${{{var}")) {
                    return Some("$(...) command substitution");
                }
            }
            i = j + 1;
            continue;
        }
        if chars[i] == '`' {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != '`' {
                j += 1;
            }
            let inner: String = chars[start..j].iter().collect();
            for var in env_vars {
                if inner.contains(&format!("${var}")) || inner.contains(&format!("${{{var}")) {
                    return Some("backtick command substitution");
                }
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    None
}

fn has_eval(line: &str) -> bool {
    let bytes = line.as_bytes();
    let needle = b"eval";
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let before_ok = i == 0 || !is_word_char(bytes[i - 1]);
            let after = i + needle.len();
            let after_ok = after >= bytes.len() || !is_word_char(bytes[after]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // --- extract_env_vars ---

    #[test]
    fn extract_env_vars_from_inline_label() {
        let dockerfile = r#"FROM alpine:3.21
LABEL md.sycophant.tools='[{"name":"t","description":"","args":{"q":{"type":"string","env":"QUERY"},"p":{"type":"string","env":"PAGE_ID"}}}]'
"#;
        let env = extract_env_vars(dockerfile).unwrap();
        assert!(env.contains("QUERY"));
        assert!(env.contains("PAGE_ID"));
        assert_eq!(env.len(), 2);
    }

    #[test]
    fn extract_env_vars_handles_line_continuations() {
        let dockerfile = "FROM alpine\nLABEL md.sycophant.tools='[\\\n  {\"name\":\"t\",\"args\":{\"q\":{\"type\":\"string\",\"env\":\"QUERY\"}}}\\\n]'\n";
        let env = extract_env_vars(dockerfile).unwrap();
        assert!(env.contains("QUERY"));
    }

    #[test]
    fn extract_env_vars_missing_label_errors() {
        let err = extract_env_vars("FROM alpine\n").unwrap_err();
        assert!(err.contains("missing `LABEL md.sycophant.tools=`"));
    }

    #[test]
    fn extract_env_vars_zero_arg_tools_yield_empty_set() {
        let dockerfile = r#"FROM alpine
LABEL md.sycophant.tools='[{"name":"t","args":{}}]'
"#;
        let env = extract_env_vars(dockerfile).unwrap();
        assert!(env.is_empty());
    }

    // --- find_var_refs ---

    #[test]
    fn find_var_refs_quoted_and_unquoted() {
        let refs = find_var_refs(r#"echo $X "$Y" ${Z} "${W}""#);
        let by_name: std::collections::HashMap<_, _> = refs
            .iter()
            .map(|r| (r.name.as_str(), r.in_double_quotes))
            .collect();
        assert_eq!(by_name["X"], false);
        assert_eq!(by_name["Y"], true);
        assert_eq!(by_name["Z"], false);
        assert_eq!(by_name["W"], true);
    }

    // --- lint_shell ---

    #[test]
    fn shell_clean_dispatch_no_diagnostics() {
        let content = r#"#!/bin/sh
set -eu
case "$1" in
    ssh-exec) exec ssh -i /key "$HOST" "$COMMAND" ;;
esac
"#;
        let diags = lint_shell(content, "dispatch", &vars(&["HOST", "COMMAND"]));
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    }

    #[test]
    fn shell_unquoted_schema_var_flagged() {
        let content = "exec ssh $HOST echo hi";
        let diags = lint_shell(content, "dispatch", &vars(&["HOST"]));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("unquoted $HOST"));
    }

    #[test]
    fn shell_unquoted_non_schema_var_not_flagged() {
        let content = "exec ssh $PATH echo hi";
        let diags = lint_shell(content, "dispatch", &vars(&["HOST"]));
        assert!(diags.is_empty());
    }

    #[test]
    fn shell_command_subst_with_schema_var_flagged() {
        let content = r#"exec echo "$(echo $QUERY)""#;
        let diags = lint_shell(content, "dispatch", &vars(&["QUERY"]));
        assert!(diags
            .iter()
            .any(|d| d.message.contains("$(...) command substitution")));
    }

    #[test]
    fn shell_backtick_with_schema_var_flagged() {
        let content = "exec echo `cat $FILE`";
        let diags = lint_shell(content, "dispatch", &vars(&["FILE"]));
        assert!(diags
            .iter()
            .any(|d| d.message.contains("backtick command substitution")));
    }

    #[test]
    fn shell_eval_flagged() {
        let content = "eval echo hi";
        let diags = lint_shell(content, "dispatch", &vars(&[]));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("eval"));
    }

    #[test]
    fn shell_comment_not_linted() {
        let content = "# this $UNQUOTED is in a comment\nexec echo ok";
        let diags = lint_shell(content, "dispatch", &vars(&["UNQUOTED"]));
        assert!(diags.is_empty());
    }

    // --- lint_makefile ---

    #[test]
    fn makefile_clean_recipe_no_diagnostics() {
        let content = "search:\n\t@ntn api v1/search -d \"$$QUERY\"\n";
        let diags = lint_makefile(content, "Makefile", &vars(&["QUERY"]));
        assert!(diags.is_empty(), "unexpected: {diags:?}");
    }

    #[test]
    fn makefile_dollarparen_schema_var_flagged() {
        let content = "search:\n\t@ntn api v1/search -d \"$(QUERY)\"\n";
        let diags = lint_makefile(content, "Makefile", &vars(&["QUERY"]));
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("make-side expansion $(QUERY)")),
            "got: {diags:?}"
        );
    }

    #[test]
    fn makefile_dollarparen_non_schema_var_not_flagged() {
        let content = "search:\n\t@echo $(SHELL)\n";
        let diags = lint_makefile(content, "Makefile", &vars(&["QUERY"]));
        assert!(diags.is_empty());
    }

    #[test]
    fn makefile_unquoted_double_dollar_var_flagged() {
        let content = "search:\n\t@ntn api v1/search -d $$QUERY\n";
        let diags = lint_makefile(content, "Makefile", &vars(&["QUERY"]));
        assert!(diags.iter().any(|d| d.message.contains("unquoted $$QUERY")));
    }

    #[test]
    fn makefile_eval_flagged() {
        let content = "search:\n\t@$(eval X = $(QUERY))\n";
        let diags = lint_makefile(content, "Makefile", &vars(&["QUERY"]));
        assert!(diags.iter().any(|d| d.message.contains("eval")));
    }

    #[test]
    fn makefile_non_recipe_lines_ignored() {
        let content = "QUERY = oops\n.PHONY: search\n";
        let diags = lint_makefile(content, "Makefile", &vars(&["QUERY"]));
        assert!(diags.is_empty());
    }
}
