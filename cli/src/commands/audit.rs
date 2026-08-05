//! `syco tenant audit <workspace> --ns <t>` — assert a running workspace upholds
//! the security clauses. These are mechanical probes (kubectl exec/logs/get +
//! one transient probe pod), not model-output evaluation: is egress actually
//! fenced, is the LLM key actually absent from the sandbox, was it actually
//! scrubbed from the conversation log. The workspace must already have been
//! exercised so the lazy-spawned stdlib chamber pod exists to probe.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cli::AuditCmd;
use crate::commands::common::{ok, step, warn};
use crate::runner::{run_output, run_passthrough, run_silent};
use crate::scope::Scope;

// Real key prefixes + a length floor, so the bare strings "sk-"/"sk-ant-" in
// ordinary text don't false-positive.
const KEY_REGEX: &str = "sk-ant-[A-Za-z0-9_-]{50,}|sk-[A-Za-z0-9_-]{40,}";

/// Name prefix for the throwaway ephemeral container that scans the conv log.
const SCRUB_CONTAINER_PREFIX: &str = "syco-audit-scrub";

enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

pub(crate) fn run(scope: &Scope, cmd: AuditCmd) -> Result<(), String> {
    let ns = scope.release_name()?;
    let ns = ns.as_str();
    let ws = cmd.workspace.as_str();
    step(&format!(
        "Auditing workspace `{ws}` in tenant `{ns}` against the security clauses"
    ));

    let pod = chamber_pod(ns, ws)?;
    run_passthrough(
        "kubectl",
        &[
            "wait",
            "-n",
            ns,
            "--for=condition=Ready",
            "--timeout=60s",
            &format!("pod/{pod}"),
        ],
    )?;
    ok(&format!("stdlib chamber pod Ready ({pod})"));

    let mut failures = 0u32;

    // 1. gVisor kernel isolation — first dmesg line announces the sandbox.
    record(
        &mut failures,
        match run_output("kubectl", &["exec", "-n", ns, &pod, "--", "dmesg"]) {
            Ok(out) if is_gvisor_first_line(&out) => {
                Verdict::Pass("gVisor kernel isolation".into())
            }
            Ok(_) => Verdict::Fail("gVisor: first dmesg line is not 'Starting gVisor'".into()),
            Err(e) => Verdict::Fail(format!("gVisor: dmesg failed: {e}")),
        },
    );

    // 2. Secret scrubbing — no real key prefixes in harness stdout or the
    //    conversation log on the harness's own conversation-data PVC.
    record(
        &mut failures,
        match scrub_hits(ns, ws) {
            Ok((t, c)) if scrub_clean(t, c) => {
                Verdict::Pass("Secret scrubbing (0 key matches in harness + conv log)".into())
            }
            Ok((t, c)) => {
                Verdict::Fail(format!("Unscrubbed key prefixes: harness={t} conv_log={c}"))
            }
            Err(e) => Verdict::Fail(format!("Secret scrubbing probe failed: {e}")),
        },
    );

    // 3. Tool execution — the toolset controller observed a successful tool result.
    let tool_ran = run_silent(
        "sh",
        &[
            "-c",
            &format!(
                "kubectl logs -n {ns} deployment/toolset-ctrl 2>/dev/null | \
         grep -q '\"message\":\"received tool result\".*\"exit_code\":0'"
            ),
        ],
    );
    record(
        &mut failures,
        if tool_ran {
            Verdict::Pass("Tool execution (toolset controller saw exit_code=0)".into())
        } else {
            Verdict::Fail("no exit_code=0 tool result in toolset-ctrl log".into())
        },
    );

    // 4. Egress containment — the sandbox must not reach the public internet.
    let reached = run_silent(
        "kubectl",
        &[
            "exec",
            "-n",
            ns,
            &pod,
            "--",
            "wget",
            "-qO-",
            "--timeout=3",
            "https://httpbin.org/ip",
        ],
    );
    record(
        &mut failures,
        if reached {
            Verdict::Fail("egress NOT contained — stdlib chamber reached httpbin.org".into())
        } else {
            Verdict::Pass("NetworkPolicy blocks stdlib chamber egress".into())
        },
    );

    // 5. L7 DNS allowlist — arbitrary names must not resolve (DNS-tunnel guard).
    //    Best-effort: skip cleanly when the chamber image has no nslookup.
    record(
        &mut failures,
        if !run_silent(
            "kubectl",
            &[
                "exec",
                "-n",
                ns,
                &pod,
                "--",
                "sh",
                "-c",
                "command -v nslookup",
            ],
        ) {
            Verdict::Skip("L7 DNS probe skipped (nslookup absent in chamber image)".into())
        } else if run_silent(
            "kubectl",
            &["exec", "-n", ns, &pod, "--", "nslookup", "example.com"],
        ) {
            Verdict::Fail("L7 DNS allowlist NOT enforced — resolved example.com".into())
        } else {
            Verdict::Pass("L7 DNS allowlist blocks arbitrary name resolution".into())
        },
    );

    // 6. Credential isolation — the LLM key must not exist inside the sandbox.
    let key_present = run_silent(
        "kubectl",
        &[
            "exec",
            "-n",
            ns,
            &pod,
            "--",
            "cat",
            "/run/secrets/llm/api-key",
        ],
    );
    record(
        &mut failures,
        if key_present {
            Verdict::Fail("credential leak — LLM api-key present in stdlib chamber pod".into())
        } else {
            Verdict::Pass("Credential isolation (no LLM key in stdlib chamber pod)".into())
        },
    );

    // 7. Workspace ServiceAccount minted.
    let sas = run_output(
        "kubectl",
        &[
            "get",
            "serviceaccounts",
            "-n",
            ns,
            "-l",
            "sycophant.md/type=workspace-sa",
            "-o",
            "name",
        ],
    )
    .unwrap_or_default();
    record(
        &mut failures,
        if workspace_sa_present(&sas, ws) {
            Verdict::Pass("Workspace ServiceAccount present".into())
        } else {
            Verdict::Fail(format!("sa-{ws} ServiceAccount missing"))
        },
    );

    if failures == 0 {
        ok("audit passed — all security clauses hold");
        Ok(())
    } else {
        Err(format!("audit FAILED — {failures} clause(s) violated"))
    }
}

fn record(failures: &mut u32, verdict: Verdict) {
    match verdict {
        Verdict::Pass(m) => ok(&m),
        Verdict::Skip(m) => warn(&m),
        Verdict::Fail(m) => {
            warn(&m);
            *failures += 1;
        }
    }
}

/// The lazy-spawned stdlib chamber pod for `ws`. Absent until the agent runs a
/// tool, so a missing pod means the workspace was never exercised — fail loud
/// with the fix rather than silently passing.
fn chamber_pod(ns: &str, ws: &str) -> Result<String, String> {
    let selector = format!(
        "app.kubernetes.io/component=airlock-job,sycophant.md/workspace={ws},sycophant.md/toolset=stdlib"
    );
    let pod = run_output(
        "kubectl",
        &[
            "get",
            "pod",
            "-n",
            ns,
            "-l",
            &selector,
            "-o",
            "jsonpath={.items[0].metadata.name}",
        ],
    )
    .unwrap_or_default();
    if pod.is_empty() {
        return Err(format!(
            "stdlib chamber pod for workspace `{ws}` not found.\n  \
             The audit probes a live sandbox: send the agent a message that triggers a\n  \
             tool call (via an enrolled client) so the chamber pod spawns, then re-run."
        ));
    }
    Ok(pod)
}

/// Key-prefix hit counts in the two sinks: harness stdout and the
/// conversation log (scanned via an ephemeral container attached to the
/// harness pod — see `scan_conv_log`).
fn scrub_hits(ns: &str, ws: &str) -> Result<(u32, u32), String> {
    let harness = run_output(
        "sh",
        &[
            "-c",
            &format!(
                "kubectl logs -n {ns} deployment/{ws} -c harness --tail=10000 2>/dev/null | \
         grep -cE '{KEY_REGEX}' || true"
            ),
        ],
    )
    .unwrap_or_default();
    let conv = scan_conv_log(ns, ws)?;
    Ok((parse_grep_count(&harness), conv))
}

fn scan_conv_log(ns: &str, ws: &str) -> Result<u32, String> {
    let pod = harness_pod(ns, ws)?;
    // Ephemeral containers can't be removed and a duplicate name fails to add,
    // so each run uses a fresh nonce-suffixed name.
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let container = format!("{SCRUB_CONTAINER_PREFIX}-{nonce}");
    // The conv log is on the harness's RWO PVC, already mounted by the
    // running harness — a separate pod can't mount it. Attach an ephemeral
    // busybox sharing the harness's PID namespace and read the dir via
    // /proc/1/root (the harness image is FROM scratch, no shell of its own).
    let patch = serde_json::json!({
        "spec": { "ephemeralContainers": [{
            "name": container,
            "image": "busybox:1.36",
            "command": ["sleep", "60"],
            "targetContainerName": "harness",
            "securityContext": {
                "runAsNonRoot": true,
                "runAsUser": 1000,
                "readOnlyRootFilesystem": true,
                "allowPrivilegeEscalation": false,
                "capabilities": { "drop": ["ALL"] },
                "seccompProfile": { "type": "RuntimeDefault" }
            }
        }]}
    })
    .to_string();
    run_output(
        "kubectl",
        &[
            "patch",
            "pod",
            &pod,
            "-n",
            ns,
            "--subresource=ephemeralcontainers",
            "--type=strategic",
            "-p",
            &patch,
        ],
    )?;
    // The exec fails until the ephemeral container is running; poll, then count
    // conv-log files containing a key (grep -c per file, drop the `:0` lines).
    let grep = format!(
        "grep -rcE '{KEY_REGEX}' /proc/1/root/var/lib/harness/conversations 2>/dev/null | grep -v ':0$' | wc -l"
    );
    let mut last_err = String::new();
    for _ in 0..15 {
        match run_output(
            "kubectl",
            &[
                "exec", "-n", ns, &pod, "-c", &container, "--", "sh", "-c", &grep,
            ],
        ) {
            Ok(out) => return Ok(parse_grep_count(&out)),
            Err(e) => {
                last_err = e;
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
    Err(format!(
        "conv-log scrub scan never became ready: {last_err}"
    ))
}

/// The harness pod serving `ws` (one harness per workspace).
fn harness_pod(ns: &str, ws: &str) -> Result<String, String> {
    let selector = format!("app.kubernetes.io/component=harness,sycophant.md/workspace={ws}");
    let pod = run_output(
        "kubectl",
        &[
            "get",
            "pod",
            "-n",
            ns,
            "-l",
            &selector,
            "-o",
            "jsonpath={.items[0].metadata.name}",
        ],
    )
    .unwrap_or_default();
    if pod.is_empty() {
        return Err(format!("harness pod for workspace `{ws}` not found"));
    }
    Ok(pod)
}

/// First dmesg line announces the gVisor sandbox.
fn is_gvisor_first_line(dmesg: &str) -> bool {
    dmesg
        .lines()
        .next()
        .map(|l| l.contains("Starting gVisor"))
        .unwrap_or(false)
}

/// Both key sinks must be empty.
fn scrub_clean(harness_hits: u32, conv_hits: u32) -> bool {
    harness_hits == 0 && conv_hits == 0
}

/// First numeric line of a grep/wc count (`grep -c`, `wc -l`), defaulting to 0.
fn parse_grep_count(s: &str) -> u32 {
    s.trim()
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or(0)
}

/// `kubectl get sa -o name` lists `serviceaccount/<name>`; the workspace SA is
/// `sa-<workspace>`. Exact line match so `sa-foo` doesn't satisfy `sa-foo-bar`.
fn workspace_sa_present(get_output: &str, ws: &str) -> bool {
    let want = format!("serviceaccount/sa-{ws}");
    get_output.lines().any(|l| l.trim() == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gvisor_first_line_matches_only_first() {
        assert!(is_gvisor_first_line("Starting gVisor...\nother\n"));
        // Mutant scanning the whole buffer (not just line 1) would pass this.
        assert!(!is_gvisor_first_line("boot\nStarting gVisor\n"));
        assert!(!is_gvisor_first_line(""));
    }

    #[test]
    fn scrub_clean_requires_both_zero() {
        // Each mutant relaxing && to || / dropping a clause is caught here.
        assert!(scrub_clean(0, 0));
        assert!(!scrub_clean(1, 0));
        assert!(!scrub_clean(0, 1));
        assert!(!scrub_clean(2, 3));
    }

    #[test]
    fn parse_grep_count_reads_first_number() {
        assert_eq!(parse_grep_count("0\n"), 0);
        assert_eq!(parse_grep_count("  3  "), 3);
        assert_eq!(parse_grep_count("5\n7\n"), 5);
        assert_eq!(parse_grep_count("not-a-number"), 0);
        assert_eq!(parse_grep_count(""), 0);
    }

    #[test]
    fn workspace_sa_exact_match() {
        let out = "serviceaccount/sa-hello-world\nserviceaccount/sa-other\n";
        assert!(workspace_sa_present(out, "hello-world"));
        assert!(workspace_sa_present(out, "other"));
        // Mutant using `contains` instead of exact match would pass this.
        assert!(!workspace_sa_present(
            "serviceaccount/sa-hello-world-2\n",
            "hello-world"
        ));
        assert!(!workspace_sa_present("", "hello-world"));
    }
}
