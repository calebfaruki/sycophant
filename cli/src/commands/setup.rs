use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::cli::SetupCmd;
use crate::commands::build;
use crate::commands::command::{COMMAND_WRITER_VERBS, SYSTEM_NS};
use crate::commands::common::{ok, step, wait_for, warn};
use crate::commands::operator;
use crate::runner::{run_output, run_passthrough, run_silent, run_stdin};
use crate::scope::Scope;

pub(crate) const CLUSTER: &str = "sycophant";
const NODE: &str = "k3d-sycophant-server-0";
const REGISTRY: &str = "sycophant-registry";
const APISERVER_AUTH_CONFIG: &str = "/etc/rancher/k3s/auth-config.yaml";

// Optional inputs for non-default clusters (the e2e harness sets them all).
// Truthy: skip the image build (the caller builds and loads its own images).
const SKIP_BUILD_ENV: &str = "SYCO_SKIP_BUILD";
// Extra `k3d cluster create` arguments, whitespace-separated.
const K3D_ARGS_ENV: &str = "SYCO_K3D_ARGS";
// A file appended to the rendered apiserver auth config before the node boots,
// e.g. a second jwt issuer.
const AUTH_CONFIG_APPEND_ENV: &str = "SYCO_AUTH_CONFIG_APPEND";
// Extra helm values files for the Kyverno engine and the cluster chart.
const KYVERNO_VALUES_ENV: &str = "SYCO_KYVERNO_VALUES";
const CLUSTER_VALUES_ENV: &str = "SYCO_CLUSTER_VALUES";
// Truthy: import the Cilium agent image from the host Docker cache before the
// install, so a slow registry pull cannot outlast the rollout wait.
const CILIUM_PRELOAD_ENV: &str = "SYCO_CILIUM_PRELOAD";

fn env_flag(name: &str) -> bool {
    env::var(name).is_ok_and(|v| !v.is_empty() && v != "0")
}

fn env_path(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

/// `-f <file>` for an optional values-file input.
fn extra_values(name: &str) -> Vec<String> {
    env_path(name)
        .map(|f| vec!["-f".to_string(), f])
        .unwrap_or_default()
}
const CILIUM_VERSION: &str = "1.19.3";
const KYVERNO_VERSION: &str = "3.9.1";
// Pinned gVisor release — the release tarball (gvisor.tar.bz2 + .sha512) is
// verified present for aarch64 + x86_64. Bump deliberately by hand;
// `release/latest` would silently drift the node runtime.
const GVISOR_CHANNEL: &str = "release/20260914";

// Kyverno engine values: CRDs are owned by the separate kyverno-crds release, so
// the engine must install with crds.install=false (else a Kyverno reinstall
// would cascade-delete every Kyverno policy and PolicyException).
const KYVERNO_VALUES: &str = include_str!("../../values/kyverno.yaml");

// Cilium engine values: pod CIDR, kube-proxy mode, single-node operator replica
// count, and the Envoy/Hubble opt-outs. Only `k8sServiceHost` is dynamic, so it
// stays a `--set`.
const CILIUM_VALUES: &str = include_str!("../../values/cilium.yaml");

// The cluster layer requires the Kyverno CRDs, which setup installs first. Its
// idp values match the auth config the apiserver was booted with.
fn cluster_layer_helm_args(chart: &str, ca: &str, operator_id: Option<&str>) -> Vec<String> {
    [
        "upgrade",
        "--install",
        "sycophant",
        chart,
        "-n",
        SYSTEM_NS,
        "--set-string",
        &format!("idp.issuer={}", operator::ISSUER),
        "--set-string",
        &format!("idp.audience={}", operator::AUDIENCE),
        "--set-file",
        &format!("idp.certificateAuthority={ca}"),
        "--wait",
        "--timeout=5m",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(extra_values(CLUSTER_VALUES_ENV))
    .chain(
        operator_id
            .into_iter()
            .flat_map(|id| ["--set-string".to_string(), format!("selfService.operator={id}")]),
    )
    .collect()
}

/// Whether the cluster values file turns the self-service setting on.
fn self_service_enabled() -> Result<bool, String> {
    let Some(file) = env_path(CLUSTER_VALUES_ENV) else {
        return Ok(false);
    };
    let text = fs::read_to_string(&file).map_err(|e| format!("cannot read {file}: {e}"))?;
    let doc: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|e| format!("{file} is not YAML: {e}"))?;
    Ok(doc
        .get("selfService")
        .and_then(|s| s.get("enabled"))
        .and_then(serde_yaml::Value::as_bool)
        == Some(true))
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
    check_prereqs(arch.as_ref())?;
    let scope = Scope::global()?;
    // Each operator step is gated on its own result, so a partial run resumes.
    // Asked up front so the run does not stall on a prompt after the build.
    let need_dex = !operator::dex_installed();
    let need_grant = !operator::command_writer_bound(SYSTEM_NS);
    let login = if need_dex {
        Some(prompt_login()?)
    } else if need_grant || self_service_enabled()? {
        // A self-service cluster layer needs the operator's identity.
        Some(operator::credentials(&scope.oidc_dir())?)
    } else {
        None
    };
    crate::sync::extract_assets(&scope)?; // charts + version
    ok("global config scaffolded");
    ensure_cluster(&scope)?; // creates the toolset registry + instructions mount
    install_gvisor(&scope)?; // before Cilium (CRI-restart ordering)
    install_cilium()?;
    patch_coredns_registry()?; // after Cilium so CoreDNS can reschedule
    install_kyverno(&scope)?;
    match (&repo, &arch) {
        _ if env_flag(SKIP_BUILD_ENV) => {
            ok(&format!("images: {SKIP_BUILD_ENV} set — skipping build"))
        }
        (Some(repo), Some(arch)) => build::build_and_load(repo, arch)?,
        _ => ok("images: no repo checkout — skipping build (expecting prebuilt images)"),
    }
    let identity = match login {
        Some((email, password)) => Some(install_operator(&scope, need_dex, email, password)?),
        None => None,
    };
    install_cluster_layer(&scope, identity.as_deref())?;
    install_objectstore(&scope)?;
    create_instructions_bucket()?;
    eprintln!(
        "\n\x1b[1;32m==> Cluster ready.\x1b[0m Next: `sycoadm tenant up -f <tenant values.yaml>`."
    );
    Ok(())
}

fn prompt_login() -> Result<(String, String), String> {
    eprintln!("Dex operator login (the password is kept only as a bcrypt hash in the cluster)");
    let email = operator::prompt("Operator email", operator::EMAIL_ENV, false)?;
    let password = operator::prompt("Operator password", operator::PASSWORD_ENV, true)?;
    if operator::prompt("Confirm password", operator::PASSWORD_ENV, true)? != password {
        return Err("passwords do not match".into());
    }
    Ok((email, password))
}

/// Install Dex with the operator as its one password user when absent, log in
/// once to learn the identity the apiserver maps the operator to, and let it
/// write the tenant provision command. Tenant-namespace grants are made by `tenant up`
/// once the namespace exists. Returns that identity.
fn install_operator(
    scope: &Scope,
    install: bool,
    email: String,
    password: String,
) -> Result<String, String> {
    if install {
        if !operator::dex_tls_present() {
            return Err(
                "Dex TLS Secret missing from sycophant-system; run `sycoadm destroy` then `sycoadm setup`"
                    .into(),
            );
        }
        operator::install_dex(&scope.dex_chart_dir(), &email, &password)?;
        operator::save_email(&scope.oidc_dir(), &email)?;
    }
    let kube = operator::Operator::with_password(email, password)?.kubeconfig()?;
    operator::grant_command_writer(SYSTEM_NS, COMMAND_WRITER_VERBS, &kube.identity)?;
    ok(&format!(
        "{} may write the tenant provision command",
        kube.identity
    ));
    Ok(kube.identity.clone())
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
        "openssl",
        run_silent("openssl", &["version"]),
        if mac {
            "brew install openssl"
        } else {
            "apt-get install -y openssl"
        },
    );
    require(
        &mut fails,
        "htpasswd",
        run_silent("sh", &["-c", "command -v htpasswd"]),
        if mac {
            "htpasswd ships with macOS; reinstall the command-line tools"
        } else {
            "apt-get install -y apache2-utils"
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
    let oidc = scope.oidc_dir();
    let auth_config = operator::auth_config(&oidc);
    if cluster_exists(&list, CLUSTER) {
        if !auth_config.is_file() {
            return Err(format!(
                "cluster `sycophant` exists but {} is missing, so its apiserver trusts no Dex issuer; run `sycoadm destroy` then `sycoadm setup`",
                auth_config.display()
            ));
        }
        ok("cluster `sycophant` already exists");
        return Ok(());
    }
    // The apiserver reads its auth config at process start, so the trust is
    // generated before the node boots. Only the auth config is mounted.
    let tls_key = operator::generate_trust(
        &oidc,
        &scope.cluster_chart_dir(),
        &extra_values(CLUSTER_VALUES_ENV),
    )?;
    if let Some(extra) = env_path(AUTH_CONFIG_APPEND_ENV) {
        warn(&format!(
            "{AUTH_CONFIG_APPEND_ENV} is set: appending {extra} to the apiserver auth config, \
             so the apiserver trusts every issuer it adds beside Dex"
        ));
        let more =
            fs::read_to_string(&extra).map_err(|e| format!("failed to read {extra}: {e}"))?;
        let mut cfg = fs::read_to_string(&auth_config)
            .map_err(|e| format!("failed to read {}: {e}", auth_config.display()))?;
        cfg.push('\n');
        cfg.push_str(&more);
        fs::write(&auth_config, cfg)
            .map_err(|e| format!("failed to write {}: {e}", auth_config.display()))?;
    }
    let auth_mount = format!("{}:{APISERVER_AUTH_CONFIG}@server:*", auth_config.display());
    let auth_arg =
        format!("--kube-apiserver-arg=authentication-config={APISERVER_AUTH_CONFIG}@server:*");
    let dex_port = format!("127.0.0.1:{0}:{0}@loadbalancer", operator::DEX_NODEPORT);
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
    // Bind-mount the local-instructions dir into the node at the identical path, so a
    // HostPath-instructions PV's hostPath resolves inside the node. Mounts are
    // create-time only, so a cluster predating this won't have it (destroy +
    // setup to add it).
    let instructions_dir_path = scope.instructions_dir();
    fs::create_dir_all(&instructions_dir_path)
        .map_err(|e| format!("failed to create {}: {e}", instructions_dir_path.display()))?;
    let k = instructions_dir_path.to_string_lossy();
    let instructions_mount = format!("{k}:{k}@all");
    let extra = env_path(K3D_ARGS_ENV).unwrap_or_default();
    if !extra.is_empty() {
        warn(&format!(
            "{K3D_ARGS_ENV} is set: passing extra `k3d cluster create` arguments, \
             which may change the cluster's mounts, ports, and apiserver flags: {extra}"
        ));
    }
    let mut args: Vec<&str> = vec![
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
        "--k3s-arg",
        &auth_arg,
        "-v",
        &auth_mount,
        "-v",
        &instructions_mount,
        &registry_arg,
        // The host reaches Dex at the same loopback address the apiserver does.
        "--port",
        &dex_port,
        "--wait",
    ];
    args.extend(extra.split_whitespace());
    run_passthrough("k3d", &args)?;
    ok("cluster `sycophant` created");
    // Dex's TLS key goes from memory into the cluster and nowhere else.
    apply_system_ns(scope)?;
    operator::upload_tls(&oidc, &tls_key)?;
    ok("Dex TLS Secret stored in sycophant-system");
    Ok(())
}

/// Teach CoreDNS to resolve `sycophant-registry` (the registry container lives on
/// the k3d Docker network, not in Kubernetes Services) so in-cluster clients can
/// fetch toolset image manifests from it. Self-guarded + idempotent; must run
/// after Cilium so the rescheduled CoreDNS pods can get IPs.
fn patch_coredns_registry() -> Result<(), String> {
    step("Wiring CoreDNS for sycophant-registry");
    let tmpl = format!("{{{{ (index .NetworkSettings.Networks \"k3d-{CLUSTER}\").IPAddress }}}}");
    let ip = run_output("docker", &["inspect", REGISTRY, "--format", &tmpl]).unwrap_or_default();
    if ip.is_empty() {
        warn(&format!(
            "could not resolve the {REGISTRY} IP on the k3d-{CLUSTER} network — skipping CoreDNS wiring; toolset image pulls from {REGISTRY}:5000 will fail"
        ));
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
    // No restart: the k3s Corefile reloads NodeHosts on its own within about a
    // minute, and a restart would make the fresh pod re-pull its image.
    ok(&format!(
        "CoreDNS NodeHosts maps sycophant-registry -> {ip}"
    ));
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

    for f in ["gvisor.tar.bz2", "gvisor.tar.bz2.sha512"] {
        run_passthrough(
            "curl",
            // --retry-all-errors also covers a mid-transfer reset, which the
            // release bucket does intermittently.
            &[
                "-sSfL",
                "--retry",
                "5",
                "--retry-delay",
                "2",
                "--retry-all-errors",
                "--connect-timeout",
                "10",
                "-o",
                &format!("{t}/{f}"),
                &format!("{url}/{f}"),
            ],
        )?;
    }
    // sha512sum reads the `.sha512` file's relative name, so verify from $tmp,
    // then extract there. runsc is no longer self-contained: the tarball ships
    // gvisor-bin/ helpers that must be installed alongside runsc as a sibling.
    run_passthrough(
        "sh",
        &[
            "-c",
            &format!(
                "cd {t} && sha512sum -c gvisor.tar.bz2.sha512 && mkdir -p gvisor-extract && tar -xjf gvisor.tar.bz2 -C gvisor-extract"
            ),
        ],
    )?;
    run_passthrough(
        "chmod",
        &[
            "+x",
            &format!("{t}/gvisor-extract/runsc"),
            &format!("{t}/gvisor-extract/containerd-shim-runsc-v1"),
        ],
    )?;
    run_passthrough("docker", &["exec", NODE, "mkdir", "-p", "/usr/local/bin"])?;
    run_passthrough(
        "docker",
        &[
            "cp",
            &format!("{t}/gvisor-extract/runsc"),
            &format!("{NODE}:/usr/local/bin/runsc"),
        ],
    )?;
    run_passthrough(
        "docker",
        &[
            "cp",
            &format!("{t}/gvisor-extract/containerd-shim-runsc-v1"),
            &format!("{NODE}:/usr/local/bin/containerd-shim-runsc-v1"),
        ],
    )?;
    run_passthrough(
        "docker",
        &[
            "cp",
            &format!("{t}/gvisor-extract/gvisor-bin"),
            &format!("{NODE}:/usr/local/bin/gvisor-bin"),
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
    if env_flag(CILIUM_PRELOAD_ENV) {
        preload_cilium()?;
    }
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

/// k3d wipes the node image cache with the cluster, so the ~240MB agent image
/// is otherwise re-pulled on every fresh cluster.
fn preload_cilium() -> Result<(), String> {
    let image = format!("quay.io/cilium/cilium:v{CILIUM_VERSION}");
    run_output("docker", &["pull", "-q", &image])?;
    // Removed on drop, on failure, and on Ctrl-C.
    let tar = operator::TempFile::new("tar", "")?;
    let t = tar.path();
    run_output("docker", &["image", "save", "-o", &t, &image])?;
    run_output("k3d", &["image", "import", &t, "--cluster", CLUSTER])?;
    ok("Cilium agent image preloaded");
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
    let mut args = vec![
        "upgrade".to_string(),
        "--install".into(),
        "kyverno".into(),
        "kyverno/kyverno".into(),
        "--version".into(),
        KYVERNO_VERSION.into(),
        "-n".into(),
        "kyverno".into(),
        "-f".into(),
        values_str,
        "--wait".into(),
        "--timeout=5m".into(),
    ];
    args.extend(extra_values(KYVERNO_VALUES_ENV));
    run_passthrough("helm", &args.iter().map(String::as_str).collect::<Vec<_>>())?;
    ok("Kyverno installed");
    Ok(())
}

// Create the namespace ourselves with PSA labels — helm --create-namespace
// would land it bare, leaving privileged pods admissible. Applied from the same
// chart dir helm installs from (charts/sycophant-cluster/system-ns.yaml).
fn apply_system_ns(scope: &Scope) -> Result<(), String> {
    let ns_manifest = scope.cluster_chart_dir().join("system-ns.yaml");
    run_passthrough(
        "kubectl",
        &["apply", "-f", ns_manifest.to_string_lossy().as_ref()],
    )
}

fn install_cluster_layer(scope: &Scope, operator_id: Option<&str>) -> Result<(), String> {
    step("Installing sycophant cluster layer");
    apply_system_ns(scope)?;
    let chart = scope.cluster_chart_dir().to_string_lossy().into_owned();
    let ca = operator::ca_cert(&scope.oidc_dir())
        .to_string_lossy()
        .into_owned();
    let args = cluster_layer_helm_args(&chart, &ca, operator_id);
    run_passthrough("helm", &args.iter().map(String::as_str).collect::<Vec<_>>())?;
    ok("sycophant cluster layer installed");
    Ok(())
}

fn install_objectstore(scope: &Scope) -> Result<(), String> {
    step("Installing object store");
    let chart = scope
        .chart_dir("sycophant-objectstore")
        .to_string_lossy()
        .into_owned();
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            "sycophant-objectstore",
            &chart,
            "-n",
            SYSTEM_NS,
            "--wait",
            "--timeout=5m",
        ],
    )?;
    ok("object store installed");
    Ok(())
}

const BUCKET: &str = "sycophant-instructions";

/// Create the instructions bucket through the store's filer. A bucket is a
/// filer directory, and creating one is idempotent: 201 (created) and 409
/// (exists) both succeed. The pod turns ready on the s3 port before the filer
/// answers, so retry for a minute; any other status fails setup.
fn create_instructions_bucket() -> Result<(), String> {
    let mut last = String::new();
    wait_for(&format!("bucket {BUCKET}"), 60, || {
        let url = format!("http://localhost:8888/buckets/{BUCKET}/");
        last = run_output(
            "kubectl",
            &[
                "exec",
                "-n",
                SYSTEM_NS,
                "deploy/seaweedfs",
                "--",
                "curl",
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "-X",
                "POST",
                &url,
            ],
        )
        .unwrap_or_else(|e| e);
        last == "201" || last == "409"
    })
    .map_err(|e| format!("{e}; last answer: {last}"))
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
    fn cluster_exists_only_finds_the_named_cluster_in_valid_json() {
        let cases = [
            (r#"[{"name":"other"},{"name":"sycophant"}]"#, true),
            (r#"[{"name":"other"},{"name":"dev"}]"#, false),
            ("[]", false),
            ("not json", false),
        ];
        for (json, want) in cases {
            assert_eq!(cluster_exists(json, "sycophant"), want, "{json}");
        }
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
    fn gvisor_arch_rejects_unknown() {
        assert!(gvisor_arch("riscv64").is_err());
    }

    #[test]
    fn cluster_layer_trusts_dex() {
        let args = cluster_layer_helm_args("/c", "/oidc/ca.crt", None);
        let pairs: Vec<_> = args
            .windows(2)
            .map(|w| (w[0].as_str(), w[1].as_str()))
            .collect();
        assert!(pairs.contains(&("--set-string", "idp.issuer=https://127.0.0.1:30443/dex")));
        assert!(pairs.contains(&("--set-string", "idp.audience=sycophant-cluster")));
        assert!(pairs.contains(&("--set-file", "idp.certificateAuthority=/oidc/ca.crt")));
    }

    #[test]
    fn system_ns_manifest_carries_psa_restricted() {
        // Single source of truth: the manifest the CLI applies is the same file
        // helm installs from (charts/sycophant-cluster/system-ns.yaml).
        let manifest = include_str!("../../../charts/sycophant-cluster/system-ns.yaml");
        assert!(manifest.contains("name: sycophant-system"));
        assert!(manifest.contains("pod-security.kubernetes.io/enforce: restricted"));
        assert!(KYVERNO_NS_YAML.contains("pod-security.kubernetes.io/enforce: restricted"));
    }

    #[test]
    fn build_arch_rejects_unknown() {
        assert!(build_arch("riscv64").is_err());
    }

    #[test]
    fn is_repo_checkout_requires_both_markers() {
        // Both markers are required; one alone is not enough.
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
    fn parse_df_avail_gb_reads_fourth_column_or_none_without_data() {
        // df -Pk: Filesystem Size Used Avail Capacity Mounted — Avail is col 4.
        // 20971520 KB = 20 GB.
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted\n\
                  overlay 62725276 31000000 20971520 60% /";
        assert_eq!(parse_df_avail_gb(df), Some(20));
        assert_eq!(parse_df_avail_gb("Filesystem only header"), None);
    }
}
