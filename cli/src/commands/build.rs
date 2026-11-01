//! `syco setup` image build: cross-compile the workspace to musl, package each
//! image, load the controller images into the k3d node, and push the toolset
//! images to the local registry. Runs only when `setup` is invoked from a repo
//! checkout (the pre-published-image path); otherwise setup expects the images
//! to already exist.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::commands::common::{ok, step};
use crate::commands::setup::{BuildArch, CLUSTER};
use crate::runner::{run_passthrough, run_passthrough_in};

const REGISTRY_PUSH: &str = "localhost:5555";

/// The one toolset base image. Built first: the stdlib `toolset` image builds
/// FROM it.
const TOOLSET_BASE_TAG: &str = "toolset-base:local";

// Controller and tool-job binaries packaged from build/Dockerfile (BINARY build-arg → <name>:local).
// inference-runtime is here too: the per-call inference image is a scratch binary
// built through the generic build/Dockerfile, not a FROM-base toolset path.
const CONTROLLER_BINS: [&str; 3] = ["toolset-runtime", "relay-controller", "inference-runtime"];

// Images loaded straight into the k3d node.
const IMPORT_IMAGES: [&str; 6] = [
    "inference-runtime:local",
    "sycophant-harness:local",
    "relay-controller:local",
    "sycophant-kubectl:local",
    "sycophant-provisioner:local",
    "sycophant-caddy:local",
];

// Toolset images (built from the toolset-runtime binary) served via the registry.
const TOOLSET_IMAGES: [&str; 2] = ["toolset", "toolset-hello-world"];

/// Build context dir for a toolset image, relative to the repo root.
fn toolset_context(image: &str) -> &'static str {
    match image {
        "toolset" => "images/toolset",
        "toolset-hello-world" => "examples/sycoadm/toolsets/hello-world",
        _ => unreachable!("unknown toolset image"),
    }
}

pub(crate) fn build_and_load(repo: &Path, arch: &BuildArch) -> Result<(), String> {
    step("Building images");
    let triple = arch.rust_target;
    let darch = arch.docker_arch;

    run_passthrough_in(
        repo,
        "cargo",
        &[
            "build",
            "--release",
            "--target",
            triple,
            "-p",
            "inference-runtime",
            "-p",
            "toolset-runtime",
            "-p",
            "harness",
            "-p",
            "relay-controller",
        ],
    )?;

    // The toolset base image first: everything below builds FROM it.
    let archarg = format!("TARGETARCH={darch}");
    let staged = stage(
        repo,
        triple,
        "toolset-runtime",
        &format!("images/toolset-base/toolset-runtime-linux-{darch}"),
    )?;
    docker_build(
        repo,
        &[
            "--build-arg",
            &archarg,
            "-f",
            "images/toolset-base/Dockerfile",
            "images/toolset-base",
            "-t",
            TOOLSET_BASE_TAG,
        ],
    )?;
    let _ = fs::remove_file(&staged);

    // Controllers: stage the musl binary into the build context, package, clean up.
    for bin in CONTROLLER_BINS {
        let staged = stage(repo, triple, bin, &format!("{bin}-linux-musl-{darch}"))?;
        let binarg = format!("BINARY={bin}");
        let archarg = format!("TARGETARCH={darch}");
        let tag = format!("{bin}:local");
        docker_build(
            repo,
            &[
                "-f",
                "build/Dockerfile",
                "--build-arg",
                &binarg,
                "--build-arg",
                &archarg,
                "-t",
                &tag,
                ".",
            ],
        )?;
        let _ = fs::remove_file(&staged);
    }

    // harness packages to sycophant-harness:local (name differs from binary).
    let staged = stage(
        repo,
        triple,
        "harness",
        &format!("harness-linux-musl-{darch}"),
    )?;
    let archarg = format!("TARGETARCH={darch}");
    docker_build(
        repo,
        &[
            "-f",
            "build/Dockerfile",
            "--build-arg",
            "BINARY=harness",
            "--build-arg",
            &archarg,
            "-t",
            "sycophant-harness:local",
            ".",
        ],
    )?;
    let _ = fs::remove_file(&staged);

    // Tool toolsets: the stdlib `toolset` adds its tools on top of the base
    // image; `toolset-hello-world` builds FROM `toolset:local`, so the order of
    // TOOLSET_IMAGES matters. Each carries its tools.yaml, as one-line JSON, in
    // the md.sycophant.tools label.
    for img in TOOLSET_IMAGES {
        let ctx = toolset_context(img);
        let basearg = format!("BASE_IMAGE={TOOLSET_BASE_TAG}");
        let tag = format!("{img}:local");
        let dockerfile = format!("{ctx}/Dockerfile");
        let label = format!("md.sycophant.tools={}", tools_label(&repo.join(ctx))?);
        docker_build(
            repo,
            &[
                "--build-arg",
                &basearg,
                "--label",
                &label,
                "-f",
                &dockerfile,
                ctx,
                "-t",
                &tag,
            ],
        )?;
    }

    // kubectl helper image (no staged binary; built from its own context).
    let archarg = format!("TARGETARCH={darch}");
    docker_build(
        repo,
        &[
            "--build-arg",
            &archarg,
            "images/kubectl/",
            "-t",
            "sycophant-kubectl:local",
        ],
    )?;

    docker_build(
        repo,
        &[
            "--build-arg",
            &archarg,
            "images/caddy/",
            "-t",
            "sycophant-caddy:local",
        ],
    )?;

    // The provision command installer bakes the component charts, so its context is the repo root.
    docker_build(
        repo,
        &[
            "--build-arg",
            &archarg,
            "-f",
            "images/provisioner/Dockerfile",
            ".",
            "-t",
            "sycophant-provisioner:local",
        ],
    )?;

    step("Loading images into k3d + registry");
    for img in IMPORT_IMAGES {
        run_passthrough("k3d", &["image", "import", img, "--cluster", CLUSTER])?;
    }
    // Tool toolset images go through the local registry so the cluster can pull
    // them when a tool Job runs.
    for img in TOOLSET_IMAGES {
        let local = format!("{img}:local");
        let remote = format!("{REGISTRY_PUSH}/{img}:latest");
        run_passthrough("docker", &["tag", &local, &remote])?;
        run_passthrough("docker", &["push", &remote])?;
    }
    ok("images built + loaded");
    Ok(())
}

/// A toolset context's `tools.yaml` as one-line JSON for the image label.
/// Every value must be a quoted string. serde_yaml would coerce a plain
/// `true` or `5` into a `String`, so values are checked as YAML values.
fn tools_label(ctx: &Path) -> Result<String, String> {
    let path = ctx.join("tools.yaml");
    let raw = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let parsed: BTreeMap<String, serde_yaml::Value> =
        serde_yaml::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let flat = parsed
        .into_iter()
        .map(|(k, v)| match v {
            serde_yaml::Value::String(s) => Ok((k, s)),
            _ => Err(format!("{}: {k} is not a string", path.display())),
        })
        .collect::<Result<BTreeMap<String, String>, String>>()?;
    serde_json::to_string(&flat).map_err(|e| e.to_string())
}

/// Copy `target/<triple>/release/<src_bin>` to `<repo>/<dst_rel>` (the build
/// context), returning the staged path so the caller can remove it.
fn stage(repo: &Path, triple: &str, src_bin: &str, dst_rel: &str) -> Result<PathBuf, String> {
    let src = repo
        .join("target")
        .join(triple)
        .join("release")
        .join(src_bin);
    let dst = repo.join(dst_rel);
    fs::copy(&src, &dst).map_err(|e| format!("stage {src_bin}: {e}"))?;
    Ok(dst)
}

fn docker_build(repo: &Path, args: &[&str]) -> Result<(), String> {
    let mut full = vec!["build", "-q"];
    full.extend_from_slice(args);
    run_passthrough_in(repo, "docker", &full)
}
