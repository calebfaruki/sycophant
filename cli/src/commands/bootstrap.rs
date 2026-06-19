use std::env;
use std::fs;
use std::path::Path;

use crate::cli::BootstrapCmd;
use crate::commands::common::{find_repo_root, ok, resolve_arg, step};
use crate::runner::{run_output, run_passthrough, run_silent, run_stdin};

const DEFAULT_CILIUM_VERSION: &str = "1.19.3";
const DEFAULT_KYVERNO_VERSION: &str = "3.5.3";

// Sycophant's default substrate values. These are the swappable defaults a
// devops user may substitute by bringing their own Cilium/Kyverno (and skipping
// `syco bootstrap` entirely).
const CILIUM_VALUES: &str = include_str!("../../values/cilium.yaml");
const KYVERNO_VALUES: &str = include_str!("../../values/kyverno.yaml");

const KYVERNO_NS_YAML: &str = "apiVersion: v1
kind: Namespace
metadata:
  name: kyverno
  labels:
    pod-security.kubernetes.io/enforce: restricted
    pod-security.kubernetes.io/enforce-version: latest
    pod-security.kubernetes.io/warn: restricted
    pod-security.kubernetes.io/audit: restricted
";

/// `syco bootstrap` — install sycophant's default substrate onto a fresh cluster:
/// Cilium (CNI) and the Kyverno engine. Both are optional/substitutable — a
/// devops user with their own CNI/Kyverno skips this command. The Kyverno engine
/// is installed with `crds.install: false`; its CRDs are owned by the
/// `kyverno-crds` chart that `syco install` lays down (decoupled lifecycle).
pub(crate) fn run(cmd: BootstrapCmd) -> Result<(), String> {
    let cilium_version = resolve_arg(cmd.cilium_version, "CILIUM_VERSION", DEFAULT_CILIUM_VERSION);
    let kyverno_version =
        resolve_arg(cmd.kyverno_version, "KYVERNO_VERSION", DEFAULT_KYVERNO_VERSION);

    if !run_silent("kubectl", &["cluster-info"]) {
        return Err("kubectl can't reach a cluster".into());
    }
    if !run_silent("helm", &["version"]) {
        return Err("helm not found in PATH".into());
    }

    let repo_root = find_repo_root(Path::new("charts/kyverno-crds"))?;
    let kyverno_crds_chart = repo_root.join("charts").join("kyverno-crds");
    if !kyverno_crds_chart.is_dir() {
        return Err(format!(
            "kyverno-crds chart not found at {}. Run syco bootstrap from the sycophant repo root.",
            kyverno_crds_chart.display()
        ));
    }

    refuse_if_misplaced("Cilium", "k8s-app=cilium", "kube-system")?;
    refuse_if_misplaced("Kyverno", "app.kubernetes.io/instance=kyverno", "kyverno")?;

    step("Creating kyverno namespace with PSA restricted labels");
    run_stdin("kubectl", &["apply", "-f", "-"], KYVERNO_NS_YAML)?;
    ok("kyverno namespace ready");

    step("Refreshing upstream Helm repos");
    let _ = run_silent("helm", &["repo", "add", "cilium", "https://helm.cilium.io/"]);
    let _ = run_silent(
        "helm",
        &["repo", "add", "kyverno", "https://kyverno.github.io/kyverno/"],
    );
    run_passthrough("helm", &["repo", "update"])?;
    ok("repos up to date");

    let values_dir = env::temp_dir().join("syco-bootstrap-values");
    fs::create_dir_all(&values_dir)
        .map_err(|e| format!("failed to create {}: {e}", values_dir.display()))?;
    let cilium_values_path = values_dir.join("cilium.yaml");
    let kyverno_values_path = values_dir.join("kyverno.yaml");
    fs::write(&cilium_values_path, CILIUM_VALUES)
        .map_err(|e| format!("failed to write cilium values: {e}"))?;
    fs::write(&kyverno_values_path, KYVERNO_VALUES)
        .map_err(|e| format!("failed to write kyverno values: {e}"))?;

    step(&format!("Installing Cilium {cilium_version} into kube-system"));
    let cilium_values_str = cilium_values_path.to_string_lossy().into_owned();
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            "cilium",
            "cilium/cilium",
            "--version",
            &cilium_version,
            "-n",
            "kube-system",
            "-f",
            &cilium_values_str,
            "--wait",
            "--timeout=5m",
        ],
    )?;
    ok("Cilium installed");

    // Kyverno CRDs must land before the engine — the engine is installed with
    // crds.install=false and won't go healthy without its CRD types present.
    // Kept as a separate release so they survive a Kyverno engine reinstall.
    step("Installing kyverno-crds (Kyverno's CRD types, separate release)");
    let kyverno_crds_str = kyverno_crds_chart.to_string_lossy().into_owned();
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            "kyverno-crds",
            &kyverno_crds_str,
            "--wait",
            "--timeout=2m",
        ],
    )?;
    ok("kyverno-crds installed");

    step(&format!("Installing Kyverno {kyverno_version} engine into kyverno"));
    let kyverno_values_str = kyverno_values_path.to_string_lossy().into_owned();
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            "kyverno",
            "kyverno/kyverno",
            "--version",
            &kyverno_version,
            "-n",
            "kyverno",
            "-f",
            &kyverno_values_str,
            "--wait",
            "--timeout=5m",
        ],
    )?;
    ok("Kyverno engine installed");

    eprintln!("\n\x1b[1;32m==> Substrate ready. Next: syco install\x1b[0m");
    Ok(())
}

fn refuse_if_misplaced(name: &str, selector: &str, expected_ns: &str) -> Result<(), String> {
    let existing = run_output(
        "kubectl",
        &[
            "get",
            "pods",
            "-A",
            "-l",
            selector,
            "-o",
            "jsonpath={.items[0].metadata.namespace}",
        ],
    )
    .unwrap_or_default();
    if !existing.is_empty() && existing != expected_ns {
        return Err(format!(
            "{name} is already running in '{existing}', not {expected_ns}. Refusing to dual-install."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cilium_values_constant_is_nonempty() {
        assert!(CILIUM_VALUES.contains("ipam"));
        assert!(CILIUM_VALUES.contains("clusterPoolIPv4PodCIDRList"));
    }

    #[test]
    fn kyverno_values_disable_crds_so_install_owns_them() {
        // The engine must NOT install its own CRDs — `syco install` lays down the
        // kyverno-crds chart so the policy CRDs survive a Kyverno reinstall.
        assert!(KYVERNO_VALUES.contains("crds:"));
        assert!(KYVERNO_VALUES.contains("install: false"));
    }

    #[test]
    fn kyverno_ns_yaml_carries_psa_labels() {
        assert!(KYVERNO_NS_YAML.contains("pod-security.kubernetes.io/enforce: restricted"));
    }
}
