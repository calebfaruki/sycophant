use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::cli::SetupCmd;
use crate::commands::build;
use crate::commands::common::{ok, step};
use crate::runner::{run_output, run_passthrough, run_silent, run_stdin};
use crate::scope::Scope;

pub(crate) const CLUSTER: &str = "sycophant";
const NODE: &str = "k3d-sycophant-server-0";
const REGISTRY: &str = "sycophant-registry";
const SYSTEM_NS: &str = "sycophant-system";
const CILIUM_VERSION: &str = "1.19.3";
const KYVERNO_VERSION: &str = "3.5.3";
// Pinned gVisor release — assets verified present for aarch64 + x86_64
// (runsc, runsc.sha512, containerd-shim-runsc-v1). Bump deliberately;
// `release/latest` would silently drift the node runtime.
const GVISOR_CHANNEL: &str = "release/20260608.0";

// Kyverno engine values: CRDs are owned by the separate kyverno-crds release, so
// the engine must install with crds.install=false (else a Kyverno reinstall
// would cascade-delete every ClusterPolicy).
const KYVERNO_VALUES: &str = include_str!("../../values/kyverno.yaml");

// Cilium engine values: pod CIDR, kube-proxy mode, single-node operator replica
// count, and the Envoy/Hubble opt-outs. Only `k8sServiceHost` is dynamic, so it
// stays a `--set`.
const CILIUM_VALUES: &str = include_str!("../../values/cilium.yaml");

// The cluster layer's policyEngine is set to kyverno because setup installs
// Kyverno (P5) before this cluster-layer install (P7). Without it the chart
// fails: policyEngine has no default. Pure so the actual arg list handed to
// helm is unit + mutation testable.
fn cluster_layer_helm_args(chart: &str) -> Vec<String> {
    [
        "upgrade",
        "--install",
        "sycophant",
        chart,
        "-n",
        SYSTEM_NS,
        "--set",
        "policyEngine=kyverno",
        "--wait",
        "--timeout=5m",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

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

/// Host build target. `setup` builds images only from a repo checkout, so the
/// arch is derived (never defaulted) — a wrong default would cross-compile to
/// the wrong arch and silently produce images the cluster can't run.
pub(crate) struct BuildArch {
    pub rust_target: &'static str,
    pub docker_arch: &'static str,
    pub cross_linker: &'static str,
}

/// `syco setup` — from nothing to a sycophant-ready cluster: ensure the k3d
/// cluster `sycophant` (+ toolset registry), install the gVisor node runtime
/// (before Cilium), Cilium, wire CoreDNS for the registry, Kyverno (decoupled
/// CRDs), build + load images when run from a repo checkout, and install the
/// sycophant cluster layer. Scaffolds the global config. Idempotent. No args.
pub(crate) fn run(_cmd: SetupCmd) -> Result<(), String> {
    let repo = repo_root();
    let arch = match &repo {
        Some(_) => Some(build_arch(env::consts::ARCH)?),
        None => None,
    };
    check_prereqs(arch.as_ref())?; // P0
    let scope = Scope::global()?;
    crate::sync::extract_assets(&scope)?; // P1: charts + version
    ok("global config scaffolded");
    ensure_cluster(&scope)?; // P2 (creates the toolset registry + kernel mount)
    install_gvisor(&scope)?; // P3 — before Cilium (CRI-restart ordering)
    install_cilium()?; // P4
    patch_coredns_registry()?; // P4.5 — after Cilium so CoreDNS can reschedule
    install_kyverno(&scope)?; // P5
    match (&repo, &arch) {
        (Some(repo), Some(arch)) => build::build_and_load(repo, arch)?, // P6
        _ => ok("images: no repo checkout — skipping build (expecting prebuilt images)"),
    }
    install_cluster_layer(&scope)?; // P7
    eprintln!("\n\x1b[1;32m==> Cluster ready.\x1b[0m Next: `syco tenant up <name>`.");
    Ok(())
}

/// The repo checkout `setup` is running from, if any — i.e. a git toplevel that
/// carries the image build inputs. `None` means run the published-image path.
fn repo_root() -> Option<PathBuf> {
    let top = run_output("git", &["rev-parse", "--show-toplevel"]).ok()?;
    let root = PathBuf::from(top);
    is_repo_checkout(&root).then_some(root)
}

/// A path is a sycophant build checkout when it carries the workspace manifest
/// and the shared image Dockerfile.
fn is_repo_checkout(root: &Path) -> bool {
    root.join("Cargo.toml").is_file() && root.join("build").join("Dockerfile").is_file()
}

/// Map Rust's target arch to the musl build triple, docker arch, and cross-linker.
fn build_arch(arch: &str) -> Result<BuildArch, String> {
    match arch {
        "aarch64" => Ok(BuildArch {
            rust_target: "aarch64-unknown-linux-musl",
            docker_arch: "arm64",
            cross_linker: "aarch64-linux-musl-gcc",
        }),
        "x86_64" => Ok(BuildArch {
            rust_target: "x86_64-unknown-linux-musl",
            docker_arch: "amd64",
            cross_linker: "x86_64-linux-musl-gcc",
        }),
        other => Err(format!("unsupported build arch: {other}")),
    }
}

/// Preflight. Runtime tooling is always required; the build toolchain + disk
/// gate are checked only when `build` is set (a repo checkout). Accumulates all
/// failures and prints one fix line each, so the operator fixes them in one pass.
fn check_prereqs(build: Option<&BuildArch>) -> Result<(), String> {
    let mac = cfg!(target_os = "macos");
    let mut fails: Vec<String> = Vec::new();

    require(
        &mut fails,
        "Docker (running)",
        run_silent("docker", &["info"]),
        if mac {
            "open Docker Desktop (or: brew install --cask docker)"
        } else {
            "install Docker Engine and start it: https://docs.docker.com/engine/install/"
        },
    );
    require(
        &mut fails,
        "k3d",
        run_silent("k3d", &["version"]),
        if mac {
            "brew install k3d"
        } else {
            "curl -s https://raw.githubusercontent.com/k3d-io/k3d/main/install.sh | bash"
        },
    );
    require(
        &mut fails,
        "Helm",
        run_silent("helm", &["version"]),
        if mac {
            "brew install helm"
        } else {
            "curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash"
        },
    );
    require(
        &mut fails,
        "kubectl",
        run_silent("kubectl", &["version", "--client"]),
        if mac {
            "brew install kubectl"
        } else {
            "https://kubernetes.io/docs/tasks/tools/#kubectl"
        },
    );
    require(
        &mut fails,
        "grpcurl",
        run_silent("grpcurl", &["--version"]),
        if mac {
            "brew install grpcurl"
        } else {
            "https://github.com/fullstorydev/grpcurl/releases"
        },
    );

    if let Some(arch) = build {
        let target = arch.rust_target;
        require(
            &mut fails,
            "cargo",
            run_silent("cargo", &["--version"]),
            "https://rustup.rs",
        );
        require(
            &mut fails,
            "protoc",
            run_silent("protoc", &["--version"]),
            if mac {
                "brew install protobuf"
            } else {
                "apt-get install -y protobuf-compiler"
            },
        );
        require(
            &mut fails,
            "cmake",
            run_silent("cmake", &["--version"]),
            if mac {
                "brew install cmake"
            } else {
                "apt-get install -y cmake"
            },
        );
        if mac {
            require(
                &mut fails,
                "Xcode command-line tools",
                run_silent("xcode-select", &["-p"]),
                "xcode-select --install",
            );
        } else {
            require(
                &mut fails,
                "C compiler (cc)",
                run_silent("cc", &["--version"]),
                "apt-get install -y build-essential",
            );
        }
        let has_target = run_output("rustup", &["target", "list", "--installed"])
            .map(|o| o.lines().any(|l| l.trim() == target))
            .unwrap_or(false);
        require(
            &mut fails,
            &format!("rustup target {target}"),
            has_target,
            &format!("rustup target add {target}"),
        );
        let linker_fix = if mac {
            format!("brew install messense/macos-cross-toolchains/{target}")
        } else {
            format!(
                "install a {} cross toolchain providing {}",
                arch.docker_arch, arch.cross_linker
            )
        };
        require(
            &mut fails,
            &format!("cross-linker {}", arch.cross_linker),
            run_silent(arch.cross_linker, &["--version"]),
            &linker_fix,
        );
        require(
            &mut fails,
            &format!("~/.cargo/config.toml linker for {target}"),
            cargo_config_has_target(target),
            &format!(
                "add to ~/.cargo/config.toml:  [target.{target}]  linker = \"{}\"",
                arch.cross_linker
            ),
        );
        check_vm_disk(&mut fails);
    }

    if fails.is_empty() {
        ok("prerequisites present");
        Ok(())
    } else {
        Err(format!(
            "{} prerequisite(s) missing:\n{}",
            fails.len(),
            fails.join("\n")
        ))
    }
}

fn require(fails: &mut Vec<String>, label: &str, present: bool, fix: &str) {
    if present {
        ok(label);
    } else {
        fails.push(format!("  \u{2717} {label}\n      fix: {fix}"));
    }
}

/// True if `~/.cargo/config.toml` wires a linker for `target` (without it the
/// musl build silently links against the host cc and fails).
fn cargo_config_has_target(target: &str) -> bool {
    let dir = env::var("CARGO_HOME")
        .ok()
        .or_else(|| env::var("HOME").ok().map(|h| format!("{h}/.cargo")));
    match dir {
        Some(d) => fs::read_to_string(PathBuf::from(d).join("config.toml"))
            .map(|c| c.contains(&format!("target.{target}")))
            .unwrap_or(false),
        None => false,
    }
}

/// Gate on Docker VM free disk — the constraint is the VM, not host RAM. Below
/// ~8GB the kubelet starts imagefs-evicting mid-deploy, which reads like a
/// memory problem but isn't.
fn check_vm_disk(fails: &mut Vec<String>) {
    let Ok(out) = run_output("docker", &["run", "--rm", "busybox", "df", "-Pk", "/"]) else {
        return;
    };
    let Some(gb) = parse_df_avail_gb(&out) else {
        return;
    };
    if gb < 8 {
        fails.push(format!(
            "  \u{2717} Docker VM free disk: {gb}GB (< 8GB — kubelet will evict)\n      fix: docker system prune -af"
        ));
    } else if gb < 15 {
        ok(&format!(
            "Docker VM free disk: {gb}GB (low; consider docker system prune -af)"
        ));
    } else {
        ok(&format!("Docker VM free disk: {gb}GB"));
    }
}

/// Available GB from `df -Pk /` output (POSIX columns: Avail is the 4th).
fn parse_df_avail_gb(df_output: &str) -> Option<u64> {
    let avail_kb: u64 = df_output
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()?;
    Some(avail_kb / 1024 / 1024)
}

fn ensure_cluster(scope: &Scope) -> Result<(), String> {
    step("Ensuring k3d cluster `sycophant`");
    let list = run_output("k3d", &["cluster", "list", "-o", "json"]).unwrap_or_default();
    if cluster_exists(&list, CLUSTER) {
        ok("cluster `sycophant` already exists");
        return Ok(());
    }
    // `--registry-create` is owned-by-cluster and errors if a registry by that
    // name already exists (e.g. another k3d cluster created it). Reuse it via
    // `--registry-use` when present; create it otherwise. Either way the cluster
    // reaches it in-cluster as `sycophant-registry:5000`.
    let reg_list = run_output("k3d", &["registry", "list", "-o", "json"]).unwrap_or_default();
    let registry_arg = if registry_exists(&reg_list, REGISTRY) {
        ok("reusing existing toolset registry `sycophant-registry`");
        format!("--registry-use={REGISTRY}:5555")
    } else {
        format!("--registry-create={REGISTRY}:0.0.0.0:5555")
    };
    // Bind-mount the local-kernel dir into the node at the identical path, so a
    // HostPath-kernel PV's hostPath resolves inside the node. Mounts are
    // create-time only, so a cluster predating this won't have it (destroy +
    // setup to add it).
    let kernels = scope.kernels_dir();
    fs::create_dir_all(&kernels)
        .map_err(|e| format!("failed to create {}: {e}", kernels.display()))?;
    let k = kernels.to_string_lossy();
    let kernel_mount = format!("{k}:{k}@all");
    run_passthrough(
        "k3d",
        &[
            "cluster",
            "create",
            CLUSTER,
            "--k3s-arg",
            "--flannel-backend=none@server:*",
            "--k3s-arg",
            "--disable-network-policy@server:*",
            "--k3s-arg",
            "--disable=traefik@server:*",
            "--k3s-arg",
            "--disable=servicelb@server:*",
            "--k3s-arg",
            "--disable=metrics-server@server:*",
            "--k3s-arg",
            "--disable=helm-controller@server:*",
            "--k3s-arg",
            "--secrets-encryption@server:*",
            "-v",
            &kernel_mount,
            &registry_arg,
            "--wait",
        ],
    )?;
    ok("cluster `sycophant` created");
    Ok(())
}

/// Teach CoreDNS to resolve `sycophant-registry` (the registry container lives on
/// the k3d Docker network, not in Kubernetes Services) so toolset-controller can
/// fetch toolset image manifests for tool discovery. Self-guarded + idempotent;
/// must run after Cilium so the rescheduled CoreDNS pods can get IPs.
fn patch_coredns_registry() -> Result<(), String> {
    step("Wiring CoreDNS for sycophant-registry");
    let tmpl = format!("{{{{ (index .NetworkSettings.Networks \"k3d-{CLUSTER}\").IPAddress }}}}");
    let ip = run_output("docker", &["inspect", REGISTRY, "--format", &tmpl]).unwrap_or_default();
    if ip.is_empty() {
        ok("registry not present — skipping CoreDNS wiring");
        return Ok(());
    }
    let current = run_output(
        "kubectl",
        &[
            "get",
            "cm",
            "coredns",
            "-n",
            "kube-system",
            "-o",
            "jsonpath={.data.NodeHosts}",
        ],
    )
    .unwrap_or_default();
    if current.contains(REGISTRY) {
        ok("CoreDNS already resolves sycophant-registry");
        return Ok(());
    }
    let patch = serde_json::json!({
        "data": { "NodeHosts": format!("{current}\n{ip} {REGISTRY}") }
    })
    .to_string();
    run_passthrough(
        "kubectl",
        &[
            "patch",
            "cm",
            "coredns",
            "-n",
            "kube-system",
            "--type=merge",
            "--patch",
            &patch,
        ],
    )?;
    run_passthrough(
        "kubectl",
        &["rollout", "restart", "deploy/coredns", "-n", "kube-system"],
    )?;
    run_passthrough(
        "kubectl",
        &[
            "rollout",
            "status",
            "deploy/coredns",
            "-n",
            "kube-system",
            "--timeout=60s",
        ],
    )?;
    ok(&format!("CoreDNS resolves sycophant-registry -> {ip}"));
    Ok(())
}

fn install_gvisor(scope: &Scope) -> Result<(), String> {
    // Idempotency guard: re-HUP'ing a healthy node churns the CRI socket — the
    // exact hazard the gVisor-before-Cilium ordering protects against.
    if run_silent(
        "docker",
        &["exec", NODE, "test", "-x", "/usr/local/bin/runsc"],
    ) && run_silent("kubectl", &["get", "runtimeclass", "gvisor"])
    {
        ok("gVisor node runtime already installed");
        return Ok(());
    }
    step("Installing gVisor node runtime (before Cilium)");
    let arch = gvisor_arch(std::env::consts::ARCH)?;
    let url = format!("https://storage.googleapis.com/gvisor/releases/{GVISOR_CHANNEL}/{arch}");
    let tmp = std::env::temp_dir().join("syco-gvisor");
    fs::create_dir_all(&tmp).map_err(|e| format!("failed to create {}: {e}", tmp.display()))?;
    let t = tmp.to_string_lossy().into_owned();

    for f in [
        "runsc",
        "runsc.sha512",
        "containerd-shim-runsc-v1",
        "containerd-shim-runsc-v1.sha512",
    ] {
        run_passthrough(
            "curl",
            &["-sSfL", "-o", &format!("{t}/{f}"), &format!("{url}/{f}")],
        )?;
    }
    // sha512sum reads the `.sha512` files' relative names, so verify from $tmp.
    run_passthrough(
        "sh",
        &[
            "-c",
            &format!("cd {t} && sha512sum -c runsc.sha512 -c containerd-shim-runsc-v1.sha512"),
        ],
    )?;
    run_passthrough(
        "chmod",
        &[
            "+x",
            &format!("{t}/runsc"),
            &format!("{t}/containerd-shim-runsc-v1"),
        ],
    )?;
    run_passthrough("docker", &["exec", NODE, "mkdir", "-p", "/usr/local/bin"])?;
    run_passthrough(
        "docker",
        &[
            "cp",
            &format!("{t}/runsc"),
            &format!("{NODE}:/usr/local/bin/runsc"),
        ],
    )?;
    run_passthrough(
        "docker",
        &[
            "cp",
            &format!("{t}/containerd-shim-runsc-v1"),
            &format!("{NODE}:/usr/local/bin/containerd-shim-runsc-v1"),
        ],
    )?;
    let _ = fs::remove_dir_all(&tmp);

    let tmpl = "{{ template \"base\" . }}\n\n[plugins.\"io.containerd.cri.v1.runtime\".containerd.runtimes.runsc]\n  runtime_type = \"io.containerd.runsc.v1\"\n";
    run_stdin(
        "docker",
        &[
            "exec",
            "-i",
            NODE,
            "sh",
            "-c",
            "cat > /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.tmpl",
        ],
        tmpl,
    )?;
    run_passthrough(
        "docker",
        &["exec", NODE, "sh", "-c", "kill -HUP $(pidof k3s)"],
    )?;
    wait_healthz()?;

    let gvisor_chart = scope.gvisor_chart_dir().to_string_lossy().into_owned();
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            "sycophant-gvisor",
            &gvisor_chart,
            "--wait",
        ],
    )?;
    ok("gVisor + RuntimeClass installed");
    Ok(())
}

fn wait_healthz() -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if run_output("kubectl", &["get", "--raw", "/healthz"])
            .map(|out| out.trim() == "ok")
            .unwrap_or(false)
        {
            return Ok(());
        }
        sleep(Duration::from_secs(2));
    }
    Err("kube-apiserver did not return /healthz=ok within 120s after the gVisor HUP".into())
}

fn install_cilium() -> Result<(), String> {
    step("Installing Cilium");
    // Index the specific k3d network — ranging over all networks concatenates IPs
    // (garbage) when the node is attached to more than one Docker network.
    let tmpl = format!("{{{{ (index .NetworkSettings.Networks \"k3d-{CLUSTER}\").IPAddress }}}}");
    let api_host = run_output("docker", &["inspect", NODE, "-f", &tmpl])?;
    let _ = run_silent(
        "helm",
        &["repo", "add", "cilium", "https://helm.cilium.io/"],
    );
    run_passthrough("helm", &["repo", "update"])?;

    let values_path = std::env::temp_dir().join("syco-cilium-values.yaml");
    fs::write(&values_path, CILIUM_VALUES)
        .map_err(|e| format!("failed to write cilium values: {e}"))?;
    let values_str = values_path.to_string_lossy().into_owned();
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            "cilium",
            "cilium/cilium",
            "--version",
            CILIUM_VERSION,
            "--namespace",
            "kube-system",
            "-f",
            &values_str,
            "--set",
            &format!("k8sServiceHost={api_host}"),
            "--set",
            "k8sServicePort=6443",
            "--wait",
            "--timeout=5m",
        ],
    )?;
    ok("Cilium ready");
    Ok(())
}

fn install_kyverno(scope: &Scope) -> Result<(), String> {
    step("Installing Kyverno (CRDs, then engine)");
    run_stdin("kubectl", &["apply", "-f", "-"], KYVERNO_NS_YAML)?;
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

    let crds_chart = scope
        .kyverno_crds_chart_dir()
        .to_string_lossy()
        .into_owned();
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            "kyverno-crds",
            &crds_chart,
            "--wait",
            "--timeout=2m",
        ],
    )?;

    let values_path = std::env::temp_dir().join("syco-kyverno-values.yaml");
    fs::write(&values_path, KYVERNO_VALUES)
        .map_err(|e| format!("failed to write kyverno values: {e}"))?;
    let values_str = values_path.to_string_lossy().into_owned();
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            "kyverno",
            "kyverno/kyverno",
            "--version",
            KYVERNO_VERSION,
            "-n",
            "kyverno",
            "-f",
            &values_str,
            "--wait",
            "--timeout=5m",
        ],
    )?;
    ok("Kyverno installed");
    Ok(())
}

fn install_cluster_layer(scope: &Scope) -> Result<(), String> {
    step("Installing sycophant cluster layer");
    let chart_dir = scope.cluster_chart_dir();
    // Create the namespace ourselves with PSA labels — helm --create-namespace
    // would land it bare, leaving privileged pods admissible. Applied from the
    // same chart dir helm installs from (charts/sycophant-cluster/system-ns.yaml).
    let ns_manifest = chart_dir.join("system-ns.yaml");
    run_passthrough(
        "kubectl",
        &["apply", "-f", ns_manifest.to_string_lossy().as_ref()],
    )?;
    let chart = chart_dir.to_string_lossy().into_owned();
    let args = cluster_layer_helm_args(&chart);
    run_passthrough("helm", &args.iter().map(String::as_str).collect::<Vec<_>>())?;
    ok("sycophant cluster layer installed");
    Ok(())
}

/// True if `k3d <kind> list -o json` output contains an entry named `name`.
/// Pure so it is unit + mutation testable around the shell-out.
fn json_list_has_name(list_json: &str, name: &str) -> bool {
    serde_yaml::from_str::<serde_yaml::Value>(list_json)
        .ok()
        .and_then(|v| v.as_sequence().cloned())
        .map(|seq| {
            seq.iter()
                .any(|c| c.get("name").and_then(|n| n.as_str()) == Some(name))
        })
        .unwrap_or(false)
}

/// True if `k3d cluster list -o json` contains a cluster named `name`.
pub(crate) fn cluster_exists(list_json: &str, name: &str) -> bool {
    json_list_has_name(list_json, name)
}

/// True if `k3d registry list -o json` contains a registry named `name`.
fn registry_exists(list_json: &str, name: &str) -> bool {
    json_list_has_name(list_json, name)
}

/// Map Rust's target arch to gVisor's release-asset arch token.
fn gvisor_arch(arch: &str) -> Result<&'static str, String> {
    match arch {
        "aarch64" => Ok("aarch64"),
        "x86_64" => Ok("x86_64"),
        other => Err(format!("unsupported arch for gVisor: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_exists_true_when_named_cluster_present() {
        // Green; mutant flipping `== Some(name)` to `!=` turns this false.
        let json = r#"[{"name":"other"},{"name":"sycophant"}]"#;
        assert!(cluster_exists(json, "sycophant"));
    }

    #[test]
    fn cluster_exists_false_when_absent() {
        // Mutant dropping the name comparison (always-true) is caught here.
        let json = r#"[{"name":"other"},{"name":"dev"}]"#;
        assert!(!cluster_exists(json, "sycophant"));
    }

    #[test]
    fn cluster_exists_false_on_empty_list() {
        assert!(!cluster_exists("[]", "sycophant"));
    }

    #[test]
    fn cluster_exists_false_on_garbage() {
        // Mutant making the parse-failure path return true is caught here.
        assert!(!cluster_exists("not json", "sycophant"));
    }

    #[test]
    fn registry_exists_detects_named_registry() {
        // The reuse-vs-create branch in ensure_cluster turns on this.
        let json = r#"[{"name":"sycophant-registry","role":"registry"}]"#;
        assert!(registry_exists(json, "sycophant-registry"));
        assert!(!registry_exists(json, "other"));
        assert!(!registry_exists("[]", "sycophant-registry"));
    }

    #[test]
    fn gvisor_arch_maps_known_arches() {
        // Each mutant swapping a mapping target is caught by one of these.
        assert_eq!(gvisor_arch("aarch64").unwrap(), "aarch64");
        assert_eq!(gvisor_arch("x86_64").unwrap(), "x86_64");
    }

    #[test]
    fn gvisor_arch_rejects_unknown() {
        // Mutant turning the catch-all into Ok(...) is caught here.
        assert!(gvisor_arch("riscv64").is_err());
    }

    #[test]
    fn kyverno_values_disable_engine_crds() {
        // Engine must not own CRDs (decoupled kyverno-crds release). Mutant
        // pointing at a CRD-installing values file is caught here.
        assert!(KYVERNO_VALUES.contains("crds:"));
        assert!(KYVERNO_VALUES.contains("install: false"));
    }

    #[test]
    fn cluster_layer_selects_kyverno_policy_engine() {
        // policyEngine has no chart default, so the args install_cluster_layer
        // hands helm must carry it. Asserts the real arg list (built by the same
        // fn the call site uses) contains an adjacent `--set policyEngine=kyverno`.
        // Mutant dropping either element from that list fails here.
        let args = cluster_layer_helm_args("/charts/sycophant-cluster");
        let set_idx = args
            .iter()
            .position(|a| a == "--set")
            .expect("helm args must contain --set");
        assert_eq!(
            args.get(set_idx + 1).map(String::as_str),
            Some("policyEngine=kyverno")
        );
    }

    #[test]
    fn kyverno_ns_yaml_carries_psa_restricted() {
        assert!(KYVERNO_NS_YAML.contains("pod-security.kubernetes.io/enforce: restricted"));
    }

    #[test]
    fn system_ns_manifest_carries_psa_restricted() {
        // Single source of truth: the manifest the CLI applies is the same file
        // helm installs from (charts/sycophant-cluster/system-ns.yaml).
        let manifest = include_str!("../../../charts/sycophant-cluster/system-ns.yaml");
        assert!(manifest.contains("name: sycophant-system"));
        assert!(manifest.contains("pod-security.kubernetes.io/enforce: restricted"));
    }

    #[test]
    fn build_arch_maps_known_arches() {
        // Each mutant swapping a triple/docker-arch/linker is caught here.
        let a = build_arch("aarch64").unwrap();
        assert_eq!(a.rust_target, "aarch64-unknown-linux-musl");
        assert_eq!(a.docker_arch, "arm64");
        assert_eq!(a.cross_linker, "aarch64-linux-musl-gcc");
        let x = build_arch("x86_64").unwrap();
        assert_eq!(x.rust_target, "x86_64-unknown-linux-musl");
        assert_eq!(x.docker_arch, "amd64");
        assert_eq!(x.cross_linker, "x86_64-linux-musl-gcc");
    }

    #[test]
    fn build_arch_rejects_unknown() {
        // Mutant turning the catch-all into Ok(...) is caught here.
        assert!(build_arch("riscv64").is_err());
    }

    #[test]
    fn is_repo_checkout_requires_both_markers() {
        // Mutant flipping && to || (one marker enough) is caught here.
        let dir = std::env::temp_dir().join(format!("syco-repo-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("build")).unwrap();
        assert!(!is_repo_checkout(&dir)); // neither marker yet
        fs::write(dir.join("Cargo.toml"), "[workspace]").unwrap();
        assert!(!is_repo_checkout(&dir)); // Cargo.toml only
        fs::write(dir.join("build").join("Dockerfile"), "FROM scratch").unwrap();
        assert!(is_repo_checkout(&dir)); // both present
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_df_avail_gb_reads_fourth_column() {
        // df -Pk: Filesystem Size Used Avail Capacity Mounted — Avail is col 4.
        // 20971520 KB = 20 GB. Mutant reading a different column is caught here.
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted\n\
                  overlay 62725276 31000000 20971520 60% /";
        assert_eq!(parse_df_avail_gb(df), Some(20));
    }

    #[test]
    fn parse_df_avail_gb_none_without_data_row() {
        assert_eq!(parse_df_avail_gb("Filesystem only header"), None);
    }
}
