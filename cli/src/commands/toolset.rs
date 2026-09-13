//! Toolset tooling. `lint` reads a toolset directory's `tools.yaml`, extracts
//! the declared env-var names, and statically analyzes the dispatch and Makefile
//! files for shell-injection patterns that would let LLM-controlled arg values
//! escape the `"$VAR"` single-token boundary. `manifest` reads a BUILT image's
//! baked schema and emits the capability-manifest content the chart mounts.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use shared::toolset::ArgDecl;

use crate::cli::{ToolsetCmd, ToolsetSub};

/// Where every toolset image bakes its canonical schema. The Dockerfile
/// `COPY tools.yaml /etc/toolset/tools.yaml` puts it here; the reader reads it back.
const BAKED_SCHEMA_PATH: &str = "/etc/toolset/tools.yaml";

pub(crate) fn run(cmd: ToolsetCmd) -> Result<(), String> {
    match cmd.sub {
        ToolsetSub::Lint(c) => lint(&c.path),
        ToolsetSub::Manifest(c) => manifest(&c.image, c.toolset.as_deref(), c.grants.as_deref()),
    }
}

fn lint(dir_str: &str) -> Result<(), String> {
    let dir = Path::new(dir_str);
    if !dir.is_dir() {
        return Err(format!("not a directory: {dir_str}"));
    }

    let tools_path = dir.join("tools.yaml");
    let tools_content = fs::read_to_string(&tools_path)
        .map_err(|e| format!("failed to read {}: {e}", tools_path.display()))?;
    let env_vars = extract_env_vars(&tools_content)?;

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

/// The tool list from a `tools.yaml`, tolerating a bare top-level list or a
/// `{tools: [...]}` mapping.
fn tool_list(tools_yaml: &str) -> Result<Vec<serde_yaml::Value>, String> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(tools_yaml).map_err(|e| format!("tools.yaml parse failed: {e}"))?;
    doc.as_sequence()
        .cloned()
        .or_else(|| doc.get("tools").and_then(|t| t.as_sequence()).cloned())
        .ok_or_else(|| "tools.yaml is neither a list of tools nor a {tools: [...]} mapping".into())
}

/// Parse a `tools.yaml` and collect every declared env-var name (each arg's
/// `env` across all tools), tolerating args in list or map form.
pub(crate) fn extract_env_vars(tools_yaml: &str) -> Result<HashSet<String>, String> {
    let list = tool_list(tools_yaml)?;
    let mut env_vars = HashSet::new();
    for tool in &list {
        let args = match tool.get("args") {
            Some(serde_yaml::Value::Sequence(s)) => s.iter().collect::<Vec<_>>(),
            Some(serde_yaml::Value::Mapping(m)) => m.values().collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        for arg in args {
            if let Some(env) = arg.get("env").and_then(|e| e.as_str()) {
                env_vars.insert(env.to_string());
            }
        }
    }
    Ok(env_vars)
}

// --- manifest reader ---

/// One resolved grant merged onto the manifest's tools. Mirrors
/// `shared::toolset::CapabilityGrant`'s shape; carried here so the reader can
/// both read an operator grants file and re-emit it into the manifest.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GrantOut {
    secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    egress: Option<String>,
}

/// One tool in the emitted capability manifest. Matches the harness's
/// `ManifestTool` deserialization shape.
#[derive(Debug, Serialize)]
struct ManifestTool {
    name: String,
    description: String,
    parameters_json: String,
    toolset: String,
    args: Vec<ArgDecl>,
    grants: BTreeMap<String, GrantOut>,
}

#[derive(Debug, Serialize)]
struct Manifest {
    tools: Vec<ManifestTool>,
}

/// One tool as authored in a `tools.yaml`.
#[derive(Debug, Deserialize)]
struct RawTool {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    parameters: serde_yaml::Value,
    #[serde(default)]
    args: Vec<ArgDecl>,
}

/// Read a built image's baked schema and emit the capability-manifest content on
/// stdout. Fails closed (returns Err, printing nothing to stdout) for an image
/// that carries no baked schema.
fn manifest(
    image: &str,
    toolset_override: Option<&str>,
    grants_path: Option<&str>,
) -> Result<(), String> {
    let tools_yaml = read_baked_schema(image)?;
    let list =
        tool_list(&tools_yaml).map_err(|e| format!("baked schema in {image} is unusable: {e}"))?;
    if list.is_empty() {
        return Err(format!("baked schema in {image} names no tools"));
    }

    let toolset = toolset_override
        .map(str::to_string)
        .unwrap_or_else(|| toolset_name_from_ref(image));
    let grants = load_grants(grants_path)?;

    let mut tools = Vec::with_capacity(list.len());
    for tool in list {
        let raw: RawTool = serde_yaml::from_value(tool)
            .map_err(|e| format!("baked schema in {image} has a malformed tool: {e}"))?;
        let parameters_json = serde_json::to_string(&raw.parameters).map_err(|e| {
            format!(
                "tool {} in {image} has parameters that are not JSON-serializable: {e}",
                raw.name
            )
        })?;
        tools.push(ManifestTool {
            name: raw.name,
            description: raw.description,
            parameters_json,
            toolset: toolset.clone(),
            args: raw.args,
            grants: grants.clone(),
        });
    }

    let doc = serde_yaml::to_string(&Manifest { tools })
        .map_err(|e| format!("failed to serialize manifest: {e}"))?;
    print!("{doc}");
    Ok(())
}

/// Extract `/etc/toolset/tools.yaml` from a built image without executing it
/// (`docker create` + `docker cp` + `docker rm`). Errors when docker is absent,
/// the image is missing, or the image carries no baked schema.
fn read_baked_schema(image: &str) -> Result<String, String> {
    let create = Command::new("docker")
        .args(["create", image])
        .output()
        .map_err(|e| format!("failed to run docker: {e}"))?;
    if !create.status.success() {
        return Err(format!(
            "docker create {image} failed: {}",
            String::from_utf8_lossy(&create.stderr).trim()
        ));
    }
    let cid = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let extracted = extract_from_container(&cid, image);

    // Always remove the temporary container, regardless of the cp outcome.
    let _ = Command::new("docker").args(["rm", "-f", &cid]).output();

    extracted
}

fn extract_from_container(cid: &str, image: &str) -> Result<String, String> {
    let tmp = std::env::temp_dir().join(format!(
        "syco-baked-schema-{}-{cid}.yaml",
        std::process::id()
    ));
    let cp = Command::new("docker")
        .args(["cp", &format!("{cid}:{BAKED_SCHEMA_PATH}")])
        .arg(&tmp)
        .output()
        .map_err(|e| format!("failed to run docker cp: {e}"))?;
    if !cp.status.success() {
        return Err(format!(
            "image {image} carries no baked schema at {BAKED_SCHEMA_PATH}: {}",
            String::from_utf8_lossy(&cp.stderr).trim()
        ));
    }
    let content = fs::read_to_string(&tmp)
        .map_err(|e| format!("failed to read schema extracted from {image}: {e}"))?;
    let _ = fs::remove_file(&tmp);
    Ok(content)
}

/// Load an optional operator grants file (YAML/JSON map of grant-name ->
/// {secret, path?, egress?}). Absent path yields an empty map.
fn load_grants(path: Option<&str>) -> Result<BTreeMap<String, GrantOut>, String> {
    let Some(path) = path else {
        return Ok(BTreeMap::new());
    };
    let content =
        fs::read_to_string(path).map_err(|e| format!("failed to read grants file {path}: {e}"))?;
    serde_yaml::from_str(&content).map_err(|e| format!("failed to parse grants file {path}: {e}"))
}

/// Derive a toolset name from an image reference: strip any `@digest`, take the
/// last path segment, then drop any `:tag`.
fn toolset_name_from_ref(image: &str) -> String {
    let no_digest = image.split('@').next().unwrap_or(image);
    let last = no_digest.rsplit('/').next().unwrap_or(no_digest);
    last.split(':').next().unwrap_or(last).to_string()
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

/// Lint a shell script (e.g., the toolset `dispatch`). Flags:
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
                message: "use of `eval` is forbidden in toolset dispatchers".to_string(),
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
                message: "use of `eval` is forbidden in toolset Makefiles".to_string(),
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
/// not handle escaped quotes or single-quote contexts because toolset
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

    // --- extract_env_vars (reads tools.yaml) ---

    #[test]
    fn extract_env_vars_from_tools_yaml_list_args() {
        let tools = r#"
- name: t
  description: ""
  parameters: {type: object}
  args:
    - {name: q, type: string, env: QUERY}
    - {name: p, type: string, env: PAGE_ID}
"#;
        let env = extract_env_vars(tools).unwrap();
        assert!(env.contains("QUERY"));
        assert!(env.contains("PAGE_ID"));
        assert_eq!(env.len(), 2);
    }

    #[test]
    fn extract_env_vars_tolerates_tools_mapping_and_map_args() {
        let tools = r#"
tools:
  - name: t
    args:
      q: {type: string, env: QUERY}
"#;
        let env = extract_env_vars(tools).unwrap();
        assert!(env.contains("QUERY"));
    }

    #[test]
    fn extract_env_vars_non_catalog_yaml_errors() {
        // A bare scalar is neither a tool list nor a {tools: [...]} mapping.
        let err = extract_env_vars("42\n").unwrap_err();
        assert!(err.contains("neither a list"));
    }

    #[test]
    fn extract_env_vars_zero_arg_tools_yield_empty_set() {
        let tools = "- name: t\n  args: []\n";
        let env = extract_env_vars(tools).unwrap();
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
        assert!(!by_name["X"]);
        assert!(by_name["Y"]);
        assert!(!by_name["Z"]);
        assert!(by_name["W"]);
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
