//! `syco upgrade [--check]` — operator command: validate the cluster, upgrade
//! the cluster chart, then re-provision every tenant through its provision
//! command. No helm runs on the host against a tenant.
//!
//! Any failed cluster or tenant check exits non-zero with nothing applied, and
//! the first failure while applying stops the run. `--check` reports and
//! applies nothing. Host/toolchain prereqs live in `syco setup`, not here.

use crate::cli::UpgradeCmd;
use crate::commands::common::{ok, step, warn};
use crate::commands::operator::Operator;
use crate::commands::up;
use crate::runner::{run_output, run_passthrough, run_silent};
use crate::scope::Scope;

// Every Kyverno policy the cluster chart renders, paired with the resource that
// resolves it. A policy missing from this list is one the gate would let a
// broken cluster carry through an upgrade, so it tracks the chart's policy set
// exactly. Resources are spelled in full: the CEL policy types are split across
// four kinds and no one short name covers them.
const KYVERNO_POLICIES: &[(&str, &str)] = &[
    (
        "validatingpolicies.policies.kyverno.io",
        "cluster-capability-job-gate",
    ),
    (
        "validatingpolicies.policies.kyverno.io",
        "cluster-protect-security",
    ),
    (
        "mutatingpolicies.policies.kyverno.io",
        "cluster-capability-job-gate-envelope",
    ),
    (
        "mutatingpolicies.policies.kyverno.io",
        "cluster-runtime-class",
    ),
    ("mutatingpolicies.policies.kyverno.io", "suspend-tenant"),
    ("mutatingpolicies.policies.kyverno.io", "reap-tenant-ttl"),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-tenant-base",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-tenant-stack",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-tenant-grant",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "tenant-rolebinding-generator",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-workspace",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-workspace-grant",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "uninstall-workspace",
    ),
    ("generatingpolicies.policies.kyverno.io", "provision-user"),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-user-grant",
    ),
    ("generatingpolicies.policies.kyverno.io", "uninstall-user"),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-toolset",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-toolset-grant",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "uninstall-toolset",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-inference",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-inference-grant",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "uninstall-inference",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-channel",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "provision-channel-grant",
    ),
    (
        "generatingpolicies.policies.kyverno.io",
        "uninstall-channel",
    ),
];

pub(crate) fn run(cmd: UpgradeCmd) -> Result<(), String> {
    let scope = Scope::global()?;

    // Validate cluster + every tenant. Read-only; collect every failure before
    // deciding whether to apply.
    step("Cluster upgrade safety checks");
    let mut cluster_fails = Vec::new();
    require(
        &mut cluster_fails,
        "cluster helm release `sycophant`",
        run_silent("helm", &["status", "sycophant", "-n", "sycophant-system"]),
        "no cluster release \u{2014} run `sycoadm setup`",
    );
    require(
        &mut cluster_fails,
        "Kyverno policies ready",
        KYVERNO_POLICIES
            .iter()
            .all(|(resource, name)| policy_ready(resource, name)),
        "Kyverno policies not Ready \u{2014} check `kubectl get vpol,mpol,gpol`",
    );
    // The cleanup controller never writes `conditionStatus.ready` on a
    // DeletingPolicy, so the reap policies are checked for presence. The
    // admission webhook compiles a policy on apply, so one that exists built.
    require(
        &mut cluster_fails,
        "Kyverno scheduled policies present",
        [
            "reap-tenant",
            "reap-workspace",
            "reap-user",
            "reap-toolset",
            "reap-inference",
            "reap-channel",
        ]
        .iter()
        .all(|name| {
            run_silent(
                "kubectl",
                &["get", "deletingpolicies.policies.kyverno.io", name],
            )
        }),
        "reap policy missing \u{2014} check `kubectl get dpol`",
    );
    // No-downgrade guard: `syco upgrade` applies the charts bundled in THIS
    // binary, so a syco older than the cluster would roll the platform back.
    // (The release-present check above covers a missing release, so skew is only
    // checked when a version is readable.) Compares via version_gte, not full
    // semver precedence — pre-release ordering isn't worth a crate for `x.y.z` tags.
    if let Some(cluster_ver) = cluster_app_version() {
        let cli_ver = env!("CARGO_PKG_VERSION");
        require(
            &mut cluster_fails,
            "sycoadm not older than the cluster",
            version_gte(cli_ver, &cluster_ver),
            &format!(
                "cluster is v{cluster_ver} but this sycoadm is v{cli_ver} \u{2014} `sycoadm upgrade` \
                 would downgrade the platform; update sycoadm instead"
            ),
        );
    }

    step("Tenant re-provision checks");
    let (plans, skipped, suspended) =
        reprovision_plan(&kubectl_tenant_namespaces()?, tenant_capacity);
    for ns in &suspended {
        warn(&format!("tenant `{ns}` is suspended; skipped"));
    }
    for (ns, capacity) in &plans {
        ok(&format!("tenant `{ns}` re-provisions at capacity {capacity}"));
    }
    for (ns, _) in &skipped {
        warn(&format!("tenant `{ns}` cannot be re-provisioned"));
    }

    if let Some(msg) = summarize_failures(&cluster_fails, &[]) {
        return Err(msg);
    }
    skipped_result(&skipped)?;
    if cmd.check {
        ok(&format!(
            "cluster checks passed; all {} tenant(s) re-provisionable",
            plans.len()
        ));
        return Ok(());
    }

    // Apply. Cluster first: the upgrade ordering is enforced by this sequence,
    // not by any runtime version guard.
    //
    // Deliver THIS binary's charts, not the stale ones a prior `setup`/`up` left in
    // the config root. A new top-level `required` chart value needs a `values.yaml`
    // default, else the `--reuse-values` render below fails.
    crate::sync::extract_assets(&scope)?;
    step("Upgrading cluster");
    let dir = scope.cluster_chart_dir();
    let cluster_chart = dir.to_string_lossy().into_owned();
    // `--reuse-values` carries the operator's setup-time overrides forward; a
    // bare upgrade re-renders from chart values.yaml and drops the `idp` values
    // the schema marks `required`.
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            "sycophant",
            &cluster_chart,
            "-n",
            "sycophant-system",
            "--reuse-values",
            "--wait",
            "--timeout=5m",
        ],
    )?;
    ok("cluster upgraded");

    if !plans.is_empty() {
        let op = Operator::login(&scope.oidc_dir())?;
        let identity = &op.kubeconfig()?.identity;
        for (ns, capacity) in &plans {
            step(&format!("Re-provisioning tenant `{ns}`"));
            let tenant = Scope::for_tenant(ns)?;
            let manifest = up::tenant_manifest(&tenant, capacity, identity)?;
            up::provision(&tenant, &op, &manifest, capacity)?;
            ok(&format!("tenant `{ns}` re-provisioned"));
        }
    }
    Ok(())
}

fn skipped_result(skipped: &[(String, Vec<String>)]) -> Result<(), String> {
    if skipped.is_empty() {
        return Ok(());
    }
    let sections: Vec<String> = skipped
        .iter()
        .map(|(ns, why)| format!("tenant `{ns}`:\n{}", why.join("\n")))
        .collect();
    Err(format!(
        "{} tenant(s) cannot be re-provisioned; nothing applied:\n{}",
        skipped.len(),
        sections.join("\n")
    ))
}

/// The memory quota a provision command generated for the tenant.
fn tenant_capacity(ns: &str) -> Option<String> {
    run_output(
        "kubectl",
        &[
            "get",
            "resourcequota",
            &format!("{ns}-capacity"),
            "-n",
            ns,
            "-o",
            "jsonpath={.spec.hard.limits\\.memory}",
        ],
    )
    .ok()
    .filter(|c| !c.is_empty())
}

// Local copy of setup::require; unify only if a 3rd caller appears.
fn require(fails: &mut Vec<String>, label: &str, present: bool, detail: &str) {
    if present {
        ok(label);
    } else {
        fails.push(format!("  \u{2717} {label}\n      {detail}"));
    }
}

// -- pure helpers (unit-tested; the shell wrappers below are covered live) --

/// Split `<name> <reap-after>` lines into the tenants to re-provision with
/// their capacity, the ones that cannot be (which abort the run), and the
/// suspended ones. A suspended tenant is never re-provisioned, which would
/// restart it before its reap, and aborts nothing.
fn reprovision_plan(
    kubectl_out: &str,
    capacity: impl Fn(&str) -> Option<String>,
) -> (Vec<(String, String)>, Vec<(String, Vec<String>)>, Vec<String>) {
    let mut plans = Vec::new();
    let mut skipped = Vec::new();
    let mut suspended = Vec::new();
    for line in kubectl_out.lines() {
        let mut fields = line.split_whitespace();
        let Some(ns) = fields.next() else {
            continue;
        };
        if fields.next().is_some() {
            suspended.push(ns.to_string());
            continue;
        }
        match capacity(ns) {
            Some(c) => plans.push((ns.to_string(), c)),
            None => skipped.push((
                ns.to_string(),
                vec![format!("  \u{2717} no {ns}-capacity quota")],
            )),
        }
    }
    (plans, skipped, suspended)
}

/// Combine cluster + per-tenant validation failures into one message, or `None`
/// when everything passed.
fn summarize_failures(cluster: &[String], tenants: &[(String, Vec<String>)]) -> Option<String> {
    if cluster.is_empty() && tenants.is_empty() {
        return None;
    }
    let mut sections = Vec::new();
    if !cluster.is_empty() {
        sections.push(format!("cluster:\n{}", cluster.join("\n")));
    }
    for (ns, fails) in tenants {
        sections.push(format!("tenant `{ns}`:\n{}", fails.join("\n")));
    }
    let names: Vec<&str> = tenants.iter().map(|(ns, _)| ns.as_str()).collect();
    Some(format!(
        "aborting: upgrade validation failed ({} cluster check(s), {} tenant namespace(s): {}); nothing applied:\n{}",
        cluster.len(),
        tenants.len(),
        names.join(", "),
        sections.join("\n"),
    ))
}

// -- shell wrappers (read-only) --

fn kubectl_tenant_namespaces() -> Result<String, String> {
    run_output(
        "kubectl",
        &[
            "get",
            "ns",
            "-l",
            "app.kubernetes.io/part-of=sycophant-tenant",
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\" \"}{.metadata.annotations.sycophant\\.md/reap-after}{\"\\n\"}{end}",
        ],
    )
    .map_err(|e| format!("aborting: cannot list tenant namespaces ({e}); nothing applied"))
}

/// Readiness of one Kyverno policy, reported as a boolean at
/// `.status.conditionStatus.ready` by every kind that reports it at all.
fn policy_ready(resource: &str, name: &str) -> bool {
    run_output(
        "kubectl",
        &[
            "get",
            resource,
            name,
            "-o",
            "jsonpath={.status.conditionStatus.ready}",
        ],
    )
    .is_ok_and(|s| s.trim() == "true")
}

/// The `sycophant` cluster release's appVersion from helm metadata, or `None`
/// if the release is absent/unreadable (the release-present check covers that).
fn cluster_app_version() -> Option<String> {
    let out = run_output("helm", &["list", "-n", "sycophant-system", "-o", "json"]).ok()?;
    let releases: serde_json::Value = serde_json::from_str(&out).ok()?;
    releases
        .as_array()?
        .iter()
        .find(|r| r.get("name").and_then(serde_json::Value::as_str) == Some("sycophant"))
        .and_then(|r| r.get("app_version").and_then(serde_json::Value::as_str))
        .map(str::to_string)
}

/// True when semver `a` >= `b`, comparing numeric MAJOR.MINOR.PATCH (so 0.10 > 0.9).
/// Missing/non-numeric parts count as 0.
fn version_gte(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> (u64, u64, u64) {
        let mut it = v
            .split(['.', '-', '+'])
            .filter_map(|p| p.parse::<u64>().ok());
        (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        )
    }
    parts(a) >= parts(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unprovisionable_tenant_fails_the_run() {
        assert!(skipped_result(&[]).is_ok());
        let err = skipped_result(&[("tenant-a".into(), vec!["x".into()])]).unwrap_err();
        assert!(err.contains("tenant-a"), "{err}");
    }

    #[test]
    fn version_gte_orders_numerically_not_lexically() {
        assert!(version_gte("0.1.0", "0.1.0"), "equal is allowed");
        assert!(version_gte("0.2.0", "0.1.0"), "newer sycoadm is allowed");
        // Versions compare numerically, not as strings: a downgrade must not slip through.
        assert!(
            !version_gte("0.1.0", "0.2.0"),
            "older sycoadm must be blocked"
        );
        assert!(
            version_gte("0.10.0", "0.9.0"),
            "numeric, not lexical (0.10 > 0.9)"
        );
        assert!(version_gte("1.0.0", "0.9.9"));
    }

    #[test]
    fn a_suspended_tenant_is_left_out_of_the_reprovision_plan() {
        // One `<name> <reap-after>` line per namespace. An active tenant has
        // no reap-after, so its line ends after the name.
        let (plans, skipped, suspended) = reprovision_plan(
            "tenant-active \ntenant-gone 2026-11-01T00:00:00Z\n",
            |_| Some("s".to_string()),
        );
        assert_eq!(plans, vec![("tenant-active".to_string(), "s".to_string())]);
        assert!(skipped.is_empty());
        assert_eq!(suspended, vec!["tenant-gone"]);
    }

    #[test]
    fn summarize_failures_only_aborts_on_failure_and_names_every_one() {
        let tenants = vec![
            (
                "alpha".to_string(),
                vec!["  \u{2717} tenant helm release".to_string()],
            ),
            (
                "beta".to_string(),
                vec!["  \u{2717} data PVCs present".to_string()],
            ),
        ];
        let msg = summarize_failures(&[], &tenants).expect("must abort when any ns fails");
        // Report ALL failing namespaces, not just the first.
        assert!(msg.contains("alpha"), "missing alpha: {msg}");
        assert!(msg.contains("beta"), "missing beta: {msg}");
        assert!(
            msg.contains('2'),
            "count should reflect both failures: {msg}"
        );

        assert_eq!(summarize_failures(&[], &[]), None);
        let msg = summarize_failures(&["  \u{2717} CRDs present".to_string()], &[])
            .expect("must abort when the cluster fails");
        assert!(msg.contains("cluster"), "missing cluster section: {msg}");
    }
}
