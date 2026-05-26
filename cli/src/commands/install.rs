use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::InstallCmd;
use crate::runner::{run_output, run_passthrough, run_silent, run_stdin};

const DEFAULT_RELEASE_NAME: &str = "sycophant-quickstart";
const DEFAULT_RELEASE_NAMESPACE: &str = "default";
const DEFAULT_CILIUM_VERSION: &str = "1.19.3";
const DEFAULT_KYVERNO_VERSION: &str = "3.5.3";

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

const CILIUM_VALUES: &str = include_str!("../../values/cilium.yaml");
const KYVERNO_VALUES: &str = include_str!("../../values/kyverno.yaml");

pub(crate) fn run(cmd: InstallCmd) -> Result<(), String> {
    let release_name = resolve_arg(cmd.release_name, "RELEASE_NAME", DEFAULT_RELEASE_NAME);
    let release_namespace = resolve_arg(
        cmd.release_namespace,
        "RELEASE_NAMESPACE",
        DEFAULT_RELEASE_NAMESPACE,
    );
    let cilium_version = resolve_arg(cmd.cilium_version, "CILIUM_VERSION", DEFAULT_CILIUM_VERSION);
    let kyverno_version = resolve_arg(
        cmd.kyverno_version,
        "KYVERNO_VERSION",
        DEFAULT_KYVERNO_VERSION,
    );

    let repo_root = find_repo_root()?;
    let kyverno_crds_chart = repo_root.join("charts").join("kyverno-crds");
    let quickstart_chart = repo_root.join("charts").join("sycophant-quickstart");
    for (name, path) in [
        ("kyverno-crds chart", &kyverno_crds_chart),
        ("sycophant-quickstart chart", &quickstart_chart),
    ] {
        if !path.is_dir() {
            return Err(format!(
                "{name} not found at {}. Run syco install from the sycophant repo root.",
                path.display()
            ));
        }
    }

    if !run_silent("kubectl", &["cluster-info"]) {
        return Err("kubectl can't reach a cluster".into());
    }
    if !run_silent("helm", &["version"]) {
        return Err("helm not found in PATH".into());
    }

    refuse_if_misplaced("Cilium", "k8s-app=cilium", "kube-system")?;
    refuse_if_misplaced("Kyverno", "app.kubernetes.io/instance=kyverno", "kyverno")?;

    step("Creating kyverno namespace with PSA restricted labels");
    run_stdin("kubectl", &["apply", "-f", "-"], KYVERNO_NS_YAML)?;
    ok("kyverno namespace ready");

    step("Refreshing upstream Helm repos");
    let _ = run_silent(
        "helm",
        &["repo", "add", "cilium", "https://helm.cilium.io/"],
    );
    let _ = run_silent(
        "helm",
        &[
            "repo",
            "add",
            "kyverno",
            "https://kyverno.github.io/kyverno/",
        ],
    );
    run_passthrough("helm", &["repo", "update"])?;
    ok("repos up to date");

    let values_dir = env::temp_dir().join("syco-install-values");
    fs::create_dir_all(&values_dir)
        .map_err(|e| format!("failed to create {}: {e}", values_dir.display()))?;
    let cilium_values_path = values_dir.join("cilium.yaml");
    let kyverno_values_path = values_dir.join("kyverno.yaml");
    fs::write(&cilium_values_path, CILIUM_VALUES)
        .map_err(|e| format!("failed to write cilium values: {e}"))?;
    fs::write(&kyverno_values_path, KYVERNO_VALUES)
        .map_err(|e| format!("failed to write kyverno values: {e}"))?;

    step(&format!(
        "Installing Cilium {cilium_version} into kube-system"
    ));
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

    step("Installing kyverno-crds (sibling chart; CRDs survive Kyverno uninstall)");
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

    step(&format!(
        "Installing Kyverno {kyverno_version} into kyverno"
    ));
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
    ok("Kyverno installed");

    step(&format!(
        "Installing sycophant-quickstart (release: {release_name}) into {release_namespace}"
    ));
    helm_dependency_update(&quickstart_chart)?;
    let quickstart_chart_str = quickstart_chart.to_string_lossy().into_owned();
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            &release_name,
            &quickstart_chart_str,
            "-n",
            &release_namespace,
            "--create-namespace",
            "--wait",
            "--timeout=10m",
        ],
    )?;
    ok("sycophant-quickstart installed");

    eprintln!("\n\x1b[1;32m==> Install complete\x1b[0m");
    Ok(())
}

fn resolve_arg(flag: Option<String>, env_var: &str, default: &str) -> String {
    flag.or_else(|| env::var(env_var).ok())
        .unwrap_or_else(|| default.to_string())
}

fn find_repo_root() -> Result<PathBuf, String> {
    let cwd = env::current_dir().map_err(|e| format!("failed to get cwd: {e}"))?;
    let mut dir: &Path = cwd.as_path();
    loop {
        if dir.join("charts").join("sycophant-quickstart").is_dir() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => {
                return Err(format!(
                    "could not find sycophant repo root (looking for charts/sycophant-quickstart) starting from {}",
                    cwd.display()
                ));
            }
        }
    }
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

fn helm_dependency_update(chart_dir: &Path) -> Result<(), String> {
    let cwd = env::current_dir().map_err(|e| format!("failed to get cwd: {e}"))?;
    env::set_current_dir(chart_dir)
        .map_err(|e| format!("failed to cd into {}: {e}", chart_dir.display()))?;
    let result = run_passthrough("helm", &["dependency", "update"]);
    env::set_current_dir(&cwd).map_err(|e| format!("failed to restore cwd: {e}"))?;
    result
}

fn step(msg: &str) {
    eprintln!("\n\x1b[1;36m==> {msg}\x1b[0m");
}

fn ok(msg: &str) {
    eprintln!("\x1b[1;32m \u{2713}\x1b[0m {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_arg_prefers_flag() {
        let v = resolve_arg(Some("FROM-FLAG".into()), "NO_SUCH_VAR_XYZ", "default");
        assert_eq!(v, "FROM-FLAG");
    }

    #[test]
    fn resolve_arg_falls_back_to_default_when_no_env() {
        // Pick an env var unlikely to be set in the test environment.
        let v = resolve_arg(None, "NO_SUCH_VAR_XYZ_12345", "default");
        assert_eq!(v, "default");
    }

    #[test]
    fn cilium_values_constant_is_nonempty() {
        // Mutation-guard: catches replacing the include_str! path with empty / wrong file.
        assert!(CILIUM_VALUES.contains("ipam"));
        assert!(CILIUM_VALUES.contains("clusterPoolIPv4PodCIDRList"));
    }

    #[test]
    fn kyverno_values_constant_is_nonempty() {
        assert!(KYVERNO_VALUES.contains("crds:"));
        assert!(KYVERNO_VALUES.contains("install: false"));
    }

    #[test]
    fn kyverno_ns_yaml_carries_psa_labels() {
        assert!(KYVERNO_NS_YAML.contains("pod-security.kubernetes.io/enforce: restricted"));
        assert!(KYVERNO_NS_YAML.contains("pod-security.kubernetes.io/enforce-version: latest"));
    }
}
