use std::env;
use std::path::Path;

use crate::cli::{InstallCmd, UninstallCmd};
use crate::commands::common::{find_repo_root, ok, resolve_arg, step};
use crate::runner::{run_passthrough, run_silent};

const DEFAULT_RELEASE_NAME: &str = "sycophant-quickstart";
const DEFAULT_RELEASE_NAMESPACE: &str = "default";

/// `syco install` — install the sycophant cluster scope onto an existing cluster.
/// Cilium, Kyverno, and the gVisor node runtime are brought-beforehand substrate;
/// this command installs only sycophant's own cluster-scoped resources (CRDs,
/// RBAC, ClusterPolicies, the gVisor RuntimeClass) via the sycophant-quickstart
/// bundle. Idempotent (`helm upgrade --install`).
pub(crate) fn run(cmd: InstallCmd) -> Result<(), String> {
    let release_name = resolve_arg(cmd.release_name, "RELEASE_NAME", DEFAULT_RELEASE_NAME);
    let release_namespace = resolve_arg(
        cmd.release_namespace,
        "RELEASE_NAMESPACE",
        DEFAULT_RELEASE_NAMESPACE,
    );

    let repo_root = find_repo_root(Path::new("charts/sycophant-quickstart"))?;
    let quickstart_chart = repo_root.join("charts").join("sycophant-quickstart");
    if !quickstart_chart.is_dir() {
        return Err(format!(
            "sycophant-quickstart chart not found at {}. Run syco install from the sycophant repo root.",
            quickstart_chart.display()
        ));
    }

    if !run_silent("kubectl", &["cluster-info"]) {
        return Err("kubectl can't reach a cluster".into());
    }
    if !run_silent("helm", &["version"]) {
        return Err("helm not found in PATH".into());
    }
    require_substrate()?;

    helm_dependency_update(&quickstart_chart)?;
    let chart_str = quickstart_chart.to_string_lossy().into_owned();
    step(&format!(
        "Installing sycophant cluster scope (release: {release_name}) into {release_namespace}"
    ));
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            &release_name,
            &chart_str,
            "-n",
            &release_namespace,
            "--create-namespace",
            "--wait",
            "--timeout=5m",
        ],
    )?;
    ok("cluster scope installed");
    Ok(())
}

/// `syco uninstall` — remove the sycophant cluster scope (destructive). The
/// substrate (Cilium/Kyverno/gVisor) is left untouched.
pub(crate) fn uninstall(cmd: UninstallCmd) -> Result<(), String> {
    let release_name = resolve_arg(cmd.release_name, "RELEASE_NAME", DEFAULT_RELEASE_NAME);
    let release_namespace = resolve_arg(
        cmd.release_namespace,
        "RELEASE_NAMESPACE",
        DEFAULT_RELEASE_NAMESPACE,
    );

    if !run_silent("helm", &["status", &release_name, "-n", &release_namespace]) {
        eprintln!("Cluster scope '{release_name}' is not installed.");
        return Ok(());
    }
    step(&format!("Uninstalling sycophant cluster scope ({release_name})"));
    run_passthrough(
        "helm",
        &["uninstall", &release_name, "-n", &release_namespace],
    )?;
    ok("cluster scope removed");
    Ok(())
}

/// Verify the brought-beforehand substrate is present before installing the
/// sycophant scope, so the operator gets a clear error instead of a half-install.
fn require_substrate() -> Result<(), String> {
    if !run_silent("kubectl", &["get", "daemonset", "cilium", "-n", "kube-system"]) {
        return Err(
            "Cilium not found in kube-system. Provision your CNI (Cilium 1.19.x) before `syco install`."
                .into(),
        );
    }
    // Check the engine deployment, not the CRD: `syco install` itself installs
    // the kyverno-crds chart, so the CRD legitimately may not exist yet.
    if !run_silent(
        "kubectl",
        &["get", "deployment", "kyverno-admission-controller", "-n", "kyverno"],
    ) {
        return Err(
            "Kyverno engine not found (kyverno-admission-controller in ns kyverno). Run `syco bootstrap` or install Kyverno 3.5.x first."
                .into(),
        );
    }
    Ok(())
}

fn helm_dependency_update(chart_dir: &Path) -> Result<(), String> {
    let cwd = env::current_dir().map_err(|e| format!("failed to get cwd: {e}"))?;
    env::set_current_dir(chart_dir)
        .map_err(|e| format!("failed to cd into {}: {e}", chart_dir.display()))?;
    let result = run_passthrough("helm", &["dependency", "update"]);
    env::set_current_dir(&cwd).map_err(|e| format!("failed to restore cwd: {e}"))?;
    result
}

