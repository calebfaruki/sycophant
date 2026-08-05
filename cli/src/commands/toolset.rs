//! Toolset-build linter. Reads a toolset directory's Dockerfile, extracts
//! the LABEL's declared env-var names, and statically analyzes the dispatch
//! and Makefile files for shell-injection patterns that would let LLM-
//! controlled arg values escape the `"$VAR"` single-token boundary.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::cli::{ToolsetCmd, ToolsetList, ToolsetSet, ToolsetSub};
use crate::commands::common;
use crate::runner::{run_output, run_stdin};
use crate::scope::Scope;

pub(crate) fn run(ns: Option<&str>, cmd: ToolsetCmd) -> Result<(), String> {
    match cmd.sub {
        // `lint` operates on a local toolset directory and needs no tenant
        // scope (it takes no `--ns`).
        ToolsetSub::Lint(c) => lint(&c.path),
        ToolsetSub::Set(c) => do_set(&tenant_scope(ns)?, c),
        ToolsetSub::List(c) => do_list(&tenant_scope(ns)?, c),
        ToolsetSub::Delete(c) => do_delete(&tenant_scope(ns)?, &c.name),
    }
}

fn tenant_scope(ns: Option<&str>) -> Result<Scope, String> {
    Scope::for_tenant(ns.ok_or_else(|| "--ns <name> is required for this command".to_string())?)
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolsetEntry {
    pub name: String,
    pub image: String,
    pub keepalive: String,
}

/// A parsed `--credential secret=NAME,env=VAR|file=PATH` mapping. Exactly one of
/// env/file is set (mirrors the Toolset CRD's CEL XOR rule).
pub(crate) struct ParsedCredential {
    pub secret: String,
    pub env: Option<String>,
    pub file: Option<String>,
}

/// Parse `--egress domain:port` into (domain, port). Rejects empty domain and
/// non-u16 / empty port. rsplit so `host:443` works even if host had no colon.
fn parse_egress(raw: &str) -> Result<(String, u16), String> {
    let (domain, port) = raw
        .rsplit_once(':')
        .ok_or_else(|| format!("--egress '{raw}' must be domain:port"))?;
    if domain.is_empty() {
        return Err(format!("--egress '{raw}': empty domain"));
    }
    let port: u16 = port
        .parse()
        .map_err(|_| format!("--egress '{raw}': port must be 0-65535"))?;
    Ok((domain.to_string(), port))
}

/// Parse `--credential secret=NAME,env=VAR` or `secret=NAME,file=PATH`. Requires
/// `secret` and exactly one of `env`/`file` (mirrors the CRD CEL XOR).
fn parse_credential(raw: &str) -> Result<ParsedCredential, String> {
    let mut secret = None;
    let mut env = None;
    let mut file = None;
    for kv in raw.split(',') {
        let (k, v) = kv.split_once('=').ok_or_else(|| {
            format!("--credential '{raw}': expected comma-separated key=value pairs")
        })?;
        match k.trim() {
            "secret" => secret = Some(v.to_string()),
            "env" => env = Some(v.to_string()),
            "file" => file = Some(v.to_string()),
            other => return Err(format!("--credential '{raw}': unknown key '{other}'")),
        }
    }
    let secret = secret.ok_or_else(|| format!("--credential '{raw}': missing secret="))?;
    match (env.is_some(), file.is_some()) {
        (true, false) | (false, true) => Ok(ParsedCredential { secret, env, file }),
        _ => Err(format!(
            "--credential '{raw}': exactly one of env= or file= is required"
        )),
    }
}

/// Build a Toolset CR for `kubectl apply`. egress/credentials are pre-parsed and
/// validated by the caller. Operator-applied labels (`sycophant.md/type: toolset`
/// plus the `sycophant.md/toolset` selector the egress CNP keys on), never
/// helm-owned — so a platform upgrade can't prune it.
fn build_toolset_cr(
    name: &str,
    namespace: &str,
    image: Option<&str>,
    egress: &[(String, u16)],
    credentials: &[ParsedCredential],
    keepalive: bool,
) -> String {
    let name_q = serde_json::to_string(name).unwrap_or_default();
    let ns_q = serde_json::to_string(namespace).unwrap_or_default();
    let mut out = format!(
        r#"apiVersion: sycophant.md/v1
kind: Toolset
metadata:
  name: {name_q}
  namespace: {ns_q}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: toolset
    sycophant.md/toolset: {name_q}
spec:
"#
    );
    if let Some(img) = image {
        let img_q = serde_json::to_string(img).unwrap_or_default();
        out.push_str(&format!("  image: {img_q}\n"));
    }
    if !credentials.is_empty() {
        out.push_str("  credentials:\n");
        for c in credentials {
            let secret_q = serde_json::to_string(&c.secret).unwrap_or_default();
            out.push_str(&format!("    - secret: {secret_q}\n"));
            if let Some(env) = &c.env {
                let env_q = serde_json::to_string(env).unwrap_or_default();
                out.push_str(&format!("      env: {env_q}\n"));
            }
            if let Some(file) = &c.file {
                let file_q = serde_json::to_string(file).unwrap_or_default();
                out.push_str(&format!("      file: {file_q}\n"));
            }
        }
    }
    if !egress.is_empty() {
        out.push_str("  egress:\n");
        for (domain, port) in egress {
            let domain_q = serde_json::to_string(domain).unwrap_or_default();
            out.push_str(&format!("    - domain: {domain_q}\n      port: {port}\n"));
        }
    }
    out.push_str(&format!("  keepalive: {keepalive}\n"));
    out
}

/// True if `host` is a literal private/loopback/link-local IPv4 address. DNS names
/// and IPv6 return false — they take the toFQDNs path, not the toCIDR pin.
fn is_private_or_loopback_ip(host: &str) -> bool {
    host.parse::<std::net::Ipv4Addr>()
        .map(|ip| ip.is_private() || ip.is_loopback() || ip.is_link_local())
        .unwrap_or(false)
}

/// Build the per-toolset egress CiliumNetworkPolicy (`toolset-<name>`)
/// for `kubectl apply`. Authored from OUTSIDE the tenant (operator kubeconfig)
/// alongside the Toolset CR, so the in-tenant CNP-immutability invariant stays
/// absolute. Composes additively on top of the chart's `airlock-job-baseline`:
/// kube-dns:53 with an L7 `rules.dns` allowlist (toolset-ctrl FQDN + each
/// declared non-localhost, non-private-IP domain), toolset-ctrl:9090, and
/// per-entry egress (`localhost` -> toEntities, a private/loopback/link-local
/// IPv4 literal -> `toCIDR <ip>/32`, else toFQDNs) on the declared port. A
/// private IP is reached directly, not via DNS, so it is omitted from both the
/// DNS allowlist and toFQDNs. NO catch-all `matchPattern "*"` — that would
/// reopen DNS-tunnel exfil. Ports are quoted strings (Cilium requirement) — the
/// INVERSE of the Toolset CR's integer port.
fn build_toolset_egress_cnp(name: &str, namespace: &str, egress: &[(String, u16)]) -> String {
    let name_q = serde_json::to_string(name).unwrap_or_default();
    let ns_q = serde_json::to_string(namespace).unwrap_or_default();
    let cnp_name_q = serde_json::to_string(&format!("toolset-{name}")).unwrap_or_default();
    let toolset_fqdn_q =
        serde_json::to_string(&format!("toolset-ctrl.{namespace}.svc.cluster.local"))
            .unwrap_or_default();

    let mut out = format!(
        r#"apiVersion: cilium.io/v2
kind: CiliumNetworkPolicy
metadata:
  name: {cnp_name_q}
  namespace: {ns_q}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: toolset
    sycophant.md/toolset: {name_q}
spec:
  endpointSelector:
    matchLabels:
      sycophant.md/toolset: {name_q}
  egress:
    - toEndpoints:
        - matchLabels:
            io.kubernetes.pod.namespace: kube-system
            k8s-app: kube-dns
      toPorts:
        - ports:
            - port: "53"
              protocol: UDP
            - port: "53"
              protocol: TCP
          rules:
            dns:
              - matchName: {toolset_fqdn_q}
"#
    );
    for (domain, _port) in egress {
        if domain != "localhost" && !is_private_or_loopback_ip(domain) {
            let domain_q = serde_json::to_string(domain).unwrap_or_default();
            let pattern_q = serde_json::to_string(&format!("*.{domain}")).unwrap_or_default();
            out.push_str(&format!("              - matchName: {domain_q}\n"));
            out.push_str(&format!("              - matchPattern: {pattern_q}\n"));
        }
    }
    out.push_str(
        r#"    - toEndpoints:
        - matchLabels:
            app.kubernetes.io/component: toolset-ctrl
      toPorts:
        - ports:
            - port: "9090"
              protocol: TCP
"#,
    );
    for (domain, port) in egress {
        if is_private_or_loopback_ip(domain) {
            let cidr_q = serde_json::to_string(&format!("{domain}/32")).unwrap_or_default();
            out.push_str("    - toCIDR:\n");
            out.push_str(&format!("        - {cidr_q}\n"));
            out.push_str(&format!(
                "      toPorts:\n        - ports:\n            - port: \"{port}\"\n              protocol: TCP\n"
            ));
        } else if domain == "localhost" {
            out.push_str(&format!(
                "    - toEntities:\n        - localhost\n      toPorts:\n        - ports:\n            - port: \"{port}\"\n              protocol: TCP\n"
            ));
        } else {
            let domain_q = serde_json::to_string(domain).unwrap_or_default();
            let pattern_q = serde_json::to_string(&format!("*.{domain}")).unwrap_or_default();
            out.push_str(&format!(
                "    - toFQDNs:\n        - matchName: {domain_q}\n        - matchPattern: {pattern_q}\n      toPorts:\n        - ports:\n            - port: \"{port}\"\n              protocol: TCP\n"
            ));
        }
    }
    out
}

fn do_set(scope: &Scope, cmd: ToolsetSet) -> Result<(), String> {
    let egress: Vec<(String, u16)> = cmd
        .egress
        .iter()
        .map(|e| parse_egress(e))
        .collect::<Result<_, _>>()?;
    let credentials: Vec<ParsedCredential> = cmd
        .credential
        .iter()
        .map(|c| parse_credential(c))
        .collect::<Result<_, _>>()?;
    let namespace = scope.release_name()?;

    let yaml = build_toolset_cr(
        &cmd.name,
        &namespace,
        cmd.image.as_deref(),
        &egress,
        &credentials,
        cmd.keepalive,
    );
    run_stdin("kubectl", &["apply", "-n", &namespace, "-f", "-"], &yaml)?;

    // Per-toolset egress CNP, authored from outside the tenant alongside the CR
    // (the in-tenant CNP-immutability invariant stays absolute). Composes on top
    // of the chart's airlock-job-baseline fail-closed floor.
    let cnp = build_toolset_egress_cnp(&cmd.name, &namespace, &egress);
    run_stdin("kubectl", &["apply", "-n", &namespace, "-f", "-"], &cnp)?;
    eprintln!("Toolset '{}' configured.", cmd.name);
    Ok(())
}

fn do_list(scope: &Scope, cmd: ToolsetList) -> Result<(), String> {
    let namespace = scope.release_name()?;
    let output = run_output(
        "kubectl",
        &[
            "get",
            "toolsets.sycophant.md",
            "-n",
            &namespace,
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\"\\t\"}{.spec.image}{\"\\t\"}{.spec.keepalive}{\"\\n\"}{end}",
        ],
    )?;
    let entries = parse_toolset_list(&output);

    if cmd.json {
        let json =
            serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
        return Ok(());
    }

    if entries.is_empty() {
        eprintln!("No toolsets configured.");
        return Ok(());
    }

    eprintln!("{:<24} {:<12} IMAGE", "NAME", "KEEPALIVE");
    for e in &entries {
        eprintln!("{:<24} {:<12} {}", e.name, e.keepalive, e.image);
    }
    Ok(())
}

/// Parse the tab-separated `kubectl get toolsets` jsonpath output into entries.
pub(crate) fn parse_toolset_list(kubectl_output: &str) -> Vec<ToolsetEntry> {
    common::parse_tab_rows(kubectl_output)
        .iter()
        .map(|c| ToolsetEntry {
            name: common::col(c, 0),
            image: common::col(c, 1),
            keepalive: common::col(c, 2),
        })
        .collect()
}

fn do_delete(scope: &Scope, name: &str) -> Result<(), String> {
    let namespace = scope.release_name()?;
    let deleted = common::delete_cr("toolset.sycophant.md", name, &namespace)?;
    // GC the per-toolset egress CNP authored by do_set. Idempotent.
    let cnp_name = format!("toolset-{name}");
    let _ = common::delete_cr("ciliumnetworkpolicy", &cnp_name, &namespace);
    if deleted {
        eprintln!("Toolset '{name}' deleted.");
    } else {
        eprintln!("Toolset '{name}' not found.");
    }
    Ok(())
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

    // --- content-tier: build_toolset_cr / parse_egress / parse_credential ---

    fn parse_yaml(yaml: &str) -> serde_yaml::Value {
        serde_yaml::from_str(yaml).expect("builder output must be valid YAML")
    }

    #[test]
    fn toolset_cr_minimal_image_only() {
        let v = parse_yaml(&build_toolset_cr(
            "stdlib",
            "dev",
            Some("img:latest"),
            &[],
            &[],
            false,
        ));
        assert_eq!(v["kind"].as_str(), Some("Toolset"));
        assert_eq!(v["metadata"]["name"].as_str(), Some("stdlib"));
        assert_eq!(v["spec"]["image"].as_str(), Some("img:latest"));
        assert_eq!(v["spec"]["keepalive"].as_bool(), Some(false));
        assert!(v["spec"].get("credentials").is_none());
        assert!(v["spec"].get("egress").is_none());
    }

    #[test]
    fn toolset_cr_no_image_omits_image_key() {
        let v = parse_yaml(&build_toolset_cr("c", "dev", None, &[], &[], false));
        assert!(v["spec"].get("image").is_none());
    }

    #[test]
    fn toolset_cr_keepalive_true_sets_flag() {
        let v = parse_yaml(&build_toolset_cr("c", "dev", Some("i"), &[], &[], true));
        assert_eq!(v["spec"]["keepalive"].as_bool(), Some(true));
    }

    #[test]
    fn toolset_cr_egress_port_is_integer() {
        // Mutation guard: port must be a YAML integer (CRD wants uint16), not a string.
        let v = parse_yaml(&build_toolset_cr(
            "c",
            "dev",
            None,
            &[("notion.com".into(), 443)],
            &[],
            false,
        ));
        assert_eq!(
            v["spec"]["egress"][0]["domain"].as_str(),
            Some("notion.com")
        );
        assert_eq!(v["spec"]["egress"][0]["port"].as_u64(), Some(443));
        assert!(v["spec"]["egress"][0]["port"].as_str().is_none());
    }

    #[test]
    fn toolset_cr_credential_env_form_has_no_file() {
        let cred = ParsedCredential {
            secret: "s".into(),
            env: Some("VAR".into()),
            file: None,
        };
        let v = parse_yaml(&build_toolset_cr("c", "dev", None, &[], &[cred], false));
        assert_eq!(v["spec"]["credentials"][0]["secret"].as_str(), Some("s"));
        assert_eq!(v["spec"]["credentials"][0]["env"].as_str(), Some("VAR"));
        assert!(v["spec"]["credentials"][0].get("file").is_none());
    }

    #[test]
    fn toolset_cr_credential_file_form_has_no_env() {
        let cred = ParsedCredential {
            secret: "s".into(),
            env: None,
            file: Some("/p".into()),
        };
        let v = parse_yaml(&build_toolset_cr("c", "dev", None, &[], &[cred], false));
        assert_eq!(v["spec"]["credentials"][0]["file"].as_str(), Some("/p"));
        assert!(v["spec"]["credentials"][0].get("env").is_none());
    }

    #[test]
    fn toolset_cr_has_type_label_not_helm() {
        let v = parse_yaml(&build_toolset_cr("c", "dev", Some("i"), &[], &[], false));
        assert_eq!(
            v["metadata"]["labels"]["sycophant.md/type"].as_str(),
            Some("toolset")
        );
        assert_eq!(
            v["metadata"]["labels"]["sycophant.md/toolset"].as_str(),
            Some("c")
        );
        assert!(v["metadata"]["labels"]["app.kubernetes.io/managed-by"].is_null());
    }

    #[test]
    fn parse_egress_valid_and_invalid() {
        assert_eq!(
            parse_egress("notion.com:443").unwrap(),
            ("notion.com".to_string(), 443)
        );
        assert_eq!(parse_egress("host:22").unwrap().1, 22);
        assert!(parse_egress("notion.com").is_err()); // no port
        assert!(parse_egress(":443").is_err()); // empty domain
        assert!(parse_egress("notion.com:foo").is_err()); // non-numeric
        assert!(parse_egress("notion.com:99999").is_err()); // > u16
    }

    #[test]
    fn parse_credential_env_xor_file() {
        let e = parse_credential("secret=s,env=VAR").unwrap();
        assert_eq!(e.secret, "s");
        assert_eq!(e.env.as_deref(), Some("VAR"));
        assert!(e.file.is_none());
        let f = parse_credential("secret=s,file=/p").unwrap();
        assert_eq!(f.file.as_deref(), Some("/p"));
        assert!(f.env.is_none());
        assert!(parse_credential("secret=s,env=E,file=F").is_err()); // both
        assert!(parse_credential("env=E").is_err()); // no secret
        assert!(parse_credential("secret=s").is_err()); // neither
        assert!(parse_credential("secret=s,bogus=x").is_err()); // unknown key
    }

    #[test]
    fn parse_toolset_list_splits_columns() {
        let entries = parse_toolset_list("stdlib\timg:latest\ttrue\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "stdlib");
        assert_eq!(entries[0].image, "img:latest");
        assert_eq!(entries[0].keepalive, "true");
    }

    // --- content-tier: build_toolset_egress_cnp ---

    #[test]
    fn cnp_name_and_selector_target_toolset() {
        let v = parse_yaml(&build_toolset_egress_cnp("stdlib", "dev", &[]));
        assert_eq!(v["kind"].as_str(), Some("CiliumNetworkPolicy"));
        assert_eq!(v["metadata"]["name"].as_str(), Some("toolset-stdlib"));
        assert_eq!(
            v["spec"]["endpointSelector"]["matchLabels"]["sycophant.md/toolset"].as_str(),
            Some("stdlib")
        );
        assert_eq!(
            v["metadata"]["labels"]["sycophant.md/toolset"].as_str(),
            Some("stdlib")
        );
    }

    #[test]
    fn cnp_empty_egress_has_dns_and_toolset_only() {
        let v = parse_yaml(&build_toolset_egress_cnp("c", "ns1", &[]));
        let egress = v["spec"]["egress"].as_sequence().unwrap();
        assert_eq!(
            egress.len(),
            2,
            "empty egress = DNS rule + toolset-ctrl rule"
        );
        let dns = v["spec"]["egress"][0]["toPorts"][0]["rules"]["dns"]
            .as_sequence()
            .unwrap();
        assert_eq!(dns.len(), 1, "only the toolset-ctrl FQDN when egress empty");
        assert_eq!(
            dns[0]["matchName"].as_str(),
            Some("toolset-ctrl.ns1.svc.cluster.local")
        );
        assert_eq!(
            egress[1]["toEndpoints"][0]["matchLabels"]["app.kubernetes.io/component"].as_str(),
            Some("toolset-ctrl")
        );
    }

    #[test]
    fn cnp_dns_allowlist_includes_each_domain() {
        let v = parse_yaml(&build_toolset_egress_cnp(
            "c",
            "ns",
            &[("notion.com".into(), 443)],
        ));
        let dns = v["spec"]["egress"][0]["toPorts"][0]["rules"]["dns"]
            .as_sequence()
            .unwrap();
        let names: Vec<&str> = dns.iter().filter_map(|d| d["matchName"].as_str()).collect();
        let patterns: Vec<&str> = dns
            .iter()
            .filter_map(|d| d["matchPattern"].as_str())
            .collect();
        assert!(names.contains(&"toolset-ctrl.ns.svc.cluster.local"));
        assert!(names.contains(&"notion.com"));
        assert!(patterns.contains(&"*.notion.com"));
    }

    #[test]
    fn cnp_domain_becomes_tofqdns_on_declared_port() {
        let v = parse_yaml(&build_toolset_egress_cnp(
            "c",
            "ns",
            &[("github.com".into(), 22)],
        ));
        let egress = v["spec"]["egress"].as_sequence().unwrap();
        let fq = egress.last().unwrap();
        assert_eq!(fq["toFQDNs"][0]["matchName"].as_str(), Some("github.com"));
        assert_eq!(
            fq["toFQDNs"][1]["matchPattern"].as_str(),
            Some("*.github.com")
        );
        assert_eq!(fq["toPorts"][0]["ports"][0]["port"].as_str(), Some("22"));
    }

    #[test]
    fn cnp_localhost_becomes_entities_not_fqdns() {
        let yaml = build_toolset_egress_cnp("c", "ns", &[("localhost".into(), 8080)]);
        assert!(
            !yaml.contains("toFQDNs"),
            "localhost must not become a toFQDN"
        );
        let v = parse_yaml(&yaml);
        let egress = v["spec"]["egress"].as_sequence().unwrap();
        let last = egress.last().unwrap();
        assert_eq!(last["toEntities"][0].as_str(), Some("localhost"));
        assert_eq!(
            last["toPorts"][0]["ports"][0]["port"].as_str(),
            Some("8080")
        );
        let dns = v["spec"]["egress"][0]["toPorts"][0]["rules"]["dns"]
            .as_sequence()
            .unwrap();
        assert!(dns
            .iter()
            .all(|d| d["matchName"].as_str() != Some("localhost")));
    }

    #[test]
    fn cnp_private_ip_pins_cidr_not_fqdn() {
        // A private/loopback IPv4 literal is reached directly, not via DNS. It
        // must be pinned by toCIDR /32 on its declared port, absent from both
        // toFQDNs and the kube-dns allowlist.
        let yaml = build_toolset_egress_cnp("c", "ns", &[("192.168.65.254".into(), 11434)]);
        assert!(
            !yaml.contains(r#"matchName: "192.168.65.254""#),
            "private IP must not appear as a toFQDNs/DNS matchName"
        );
        assert!(
            !yaml.contains(r#"matchPattern: "*.192.168.65.254""#),
            "private IP must not emit a matchPattern"
        );
        let v = parse_yaml(&yaml);
        let egress = v["spec"]["egress"].as_sequence().unwrap();
        let cidr_rule = egress
            .iter()
            .find(|e| e.get("toCIDR").is_some())
            .expect("private IP must emit a toCIDR rule");
        let cidrs: Vec<&str> = cidr_rule["toCIDR"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|c| c.as_str())
            .collect();
        assert_eq!(cidrs, vec!["192.168.65.254/32"]);
        assert_eq!(
            cidr_rule["toPorts"][0]["ports"][0]["port"].as_str(),
            Some("11434")
        );
        assert_eq!(
            cidr_rule["toPorts"][0]["ports"][0]["protocol"].as_str(),
            Some("TCP")
        );
        let dns = v["spec"]["egress"][0]["toPorts"][0]["rules"]["dns"]
            .as_sequence()
            .unwrap();
        let names: Vec<&str> = dns.iter().filter_map(|d| d["matchName"].as_str()).collect();
        assert!(
            !names.contains(&"192.168.65.254"),
            "private IP must not land in the kube-dns allowlist"
        );
    }

    #[test]
    fn cnp_has_no_catchall() {
        let yaml = build_toolset_egress_cnp("c", "ns", &[("notion.com".into(), 443)]);
        assert!(
            !yaml.contains(r#"matchPattern: "*""#),
            "no bare DNS/FQDN catch-all"
        );
        assert!(!yaml.contains("0.0.0.0/0"));
        assert!(!yaml.contains("world"));
    }

    #[test]
    fn cnp_ports_are_quoted_strings() {
        // Mutation guard: Cilium requires string ports — the INVERSE of the
        // Toolset CR's integer port (see toolset_cr_egress_port_is_integer).
        let v = parse_yaml(&build_toolset_egress_cnp(
            "c",
            "ns",
            &[("notion.com".into(), 443)],
        ));
        let dns_port = &v["spec"]["egress"][0]["toPorts"][0]["ports"][0]["port"];
        assert_eq!(dns_port.as_str(), Some("53"));
        assert!(dns_port.as_u64().is_none());
        let egress = v["spec"]["egress"].as_sequence().unwrap();
        let fq_port = &egress.last().unwrap()["toPorts"][0]["ports"][0]["port"];
        assert_eq!(fq_port.as_str(), Some("443"));
    }
}
