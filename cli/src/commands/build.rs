//! `syco setup` image build: cross-compile the workspace to musl, package each
//! image, load the controller images into the k3d node, and push the toolset
//! images to the local registry. Runs only when `setup` is invoked from a repo
//! checkout (the pre-published-image path); otherwise setup expects the images
//! to already exist.

use std::fs;
use std::path::{Path, PathBuf};

use crate::commands::common::{ok, step};
use crate::commands::setup::{BuildArch, CLUSTER};
use crate::runner::{run_passthrough, run_passthrough_in};

const REGISTRY_PUSH: &str = "localhost:5555";

/// The one toolset base image. Built first: every toolset image, and the prompt
/// image, builds FROM it.
const TOOLSET_BASE_TAG: &str = "toolset-base:local";

// Controller and tool-job binaries packaged from build/Dockerfile (BINARY build-arg → <name>:local).
// prompt-toolset is not here: it ships as a toolset image built FROM the base.
const CONTROLLER_BINS: [&str; 3] = ["toolset-controller", "toolset-runtime", "relay-controller"];

// Images loaded straight into the k3d node. toolset-git:local is here (not only
// in the registry) because the workspace-init Job runs it node-local with
// pullPolicy=Never (chart default workspaceInit.image=toolset-git, tag=local).
const IMPORT_IMAGES: [&str; 6] = [
    "toolset-controller:local",
    "prompt-toolset:local",
    "sycophant-harness:local",
    "relay-controller:local",
    "sycophant-kubectl:local",
    "toolset-git:local",
];

// Toolset images (built from the toolset-runtime binary) served via the registry.
const TOOLSET_IMAGES: [&str; 3] = ["toolset", "toolset-git", "toolset-ssh-credentials"];

/// Build context dir for a toolset image, relative to the repo root.
fn toolset_context(image: &str) -> &'static str {
    match image {
        "toolset" => "images/toolset",
        "toolset-git" => "images/git",
        "toolset-ssh-credentials" => "examples/toolsets/ssh-credentials",
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
            "toolset-controller",
            "-p",
            "prompt-toolset",
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

    // The prompt toolset: a published image built FROM the base, not a bare
    // binary packaged through build/Dockerfile.
    let staged = stage(
        repo,
        triple,
        "prompt-toolset",
        &format!("images/prompt/prompt-toolset-linux-{darch}"),
    )?;
    let basearg = format!("BASE_IMAGE={TOOLSET_BASE_TAG}");
    let archarg = format!("TARGETARCH={darch}");
    docker_build(
        repo,
        &[
            "--build-arg",
            &basearg,
            "--build-arg",
            &archarg,
            "-f",
            "images/prompt/Dockerfile",
            "images/prompt",
            "-t",
            "prompt-toolset:local",
        ],
    )?;
    let _ = fs::remove_file(&staged);

    // Tool toolsets: each adds only its tools on top of the base image.
    for img in TOOLSET_IMAGES {
        let ctx = toolset_context(img);
        let basearg = format!("BASE_IMAGE={TOOLSET_BASE_TAG}");
        let tag = format!("{img}:local");
        let dockerfile = format!("{ctx}/Dockerfile");
        docker_build(
            repo,
            &["--build-arg", &basearg, "-f", &dockerfile, ctx, "-t", &tag],
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

    step("Loading images into k3d + registry");
    for img in IMPORT_IMAGES {
        run_passthrough("k3d", &["image", "import", img, "--cluster", CLUSTER])?;
    }
    // Tool toolset images go through the local registry so the toolset controller
    // can read their OCI manifests for tool discovery.
    for img in TOOLSET_IMAGES {
        let local = format!("{img}:local");
        let remote = format!("{REGISTRY_PUSH}/{img}:latest");
        run_passthrough("docker", &["tag", &local, &remote])?;
        run_passthrough("docker", &["push", &remote])?;
    }
    ok("images built + loaded");
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolset_contexts_map_each_image() {
        // Mutant swapping any target dir is caught here.
        assert_eq!(toolset_context("toolset"), "images/toolset");
        assert_eq!(toolset_context("toolset-git"), "images/git");
        assert_eq!(
            toolset_context("toolset-ssh-credentials"),
            "examples/toolsets/ssh-credentials"
        );
    }

    #[test]
    fn harness_is_not_a_plain_controller() {
        // It packages to sycophant-harness:local, so it must be handled
        // separately, never in the CONTROLLER_BINS <name>:local loop.
        assert!(!CONTROLLER_BINS.contains(&"harness"));
    }

    #[test]
    fn prompt_toolset_is_not_packaged_as_a_bare_binary() {
        // It ships as an image built FROM the toolset base, so the generic
        // build/Dockerfile loop must not claim it.
        assert!(!CONTROLLER_BINS.contains(&"prompt-toolset"));
        assert!(IMPORT_IMAGES.contains(&"prompt-toolset:local"));
    }

    #[test]
    fn registry_only_toolsets_are_not_imported() {
        // toolset + ssh-credentials reach the cluster ONLY via the
        // registry, never k3d image import.
        for img in ["toolset", "toolset-ssh-credentials"] {
            assert!(!IMPORT_IMAGES.contains(&format!("{img}:local").as_str()));
        }
        // toolset-git is the exception: the workspace-init Job runs it node-local
        // (pullPolicy=Never), so it is BOTH registry-pushed and k3d-imported.
        assert!(IMPORT_IMAGES.contains(&"toolset-git:local"));
        assert!(TOOLSET_IMAGES.contains(&"toolset-git"));
    }
}
