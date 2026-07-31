//! `syco setup` image build: cross-compile the workspace to musl, package each
//! image, load the controller images into the k3d node, and push the chamber
//! images to the local registry. Runs only when `setup` is invoked from a repo
//! checkout (the pre-published-image path); otherwise setup expects the images
//! to already exist.

use std::fs;
use std::path::{Path, PathBuf};

use crate::commands::common::{ok, step};
use crate::commands::setup::{BuildArch, CLUSTER};
use crate::runner::{run_passthrough, run_passthrough_in};

const REGISTRY_PUSH: &str = "localhost:5555";

// Controllers packaged from build/Dockerfile (BINARY build-arg → <name>:local).
const CONTROLLER_BINS: [&str; 6] = [
    "hangar-controller",
    "hangar-llm-job",
    "airlock-controller",
    "airlock-runtime",
    "mainframe-controller",
    "relay-controller",
];

// Images loaded straight into the k3d node. airlock-git:local is here (not only
// in the registry) because the workspace-init Job runs it node-local with
// pullPolicy=Never (chart default workspaceInit.image=airlock-git, tag=local).
const IMPORT_IMAGES: [&str; 8] = [
    "hangar-controller:local",
    "hangar-llm-job:local",
    "airlock-controller:local",
    "mainframe-controller:local",
    "sycophant-transponder:local",
    "relay-controller:local",
    "sycophant-kubectl:local",
    "airlock-git:local",
];

// Chamber images (built from the airlock-runtime binary) served via the registry.
const CHAMBER_IMAGES: [&str; 3] = ["airlock-chamber", "airlock-git", "airlock-ssh-credentials"];

/// Build context dir for a chamber image, relative to the repo root.
fn chamber_context(image: &str) -> &'static str {
    match image {
        "airlock-chamber" => "images/airlock-chamber",
        "airlock-git" => "images/git",
        "airlock-ssh-credentials" => "examples/chambers/ssh-credentials",
        _ => unreachable!("unknown chamber image"),
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
            "hangar-controller",
            "-p",
            "hangar-llm-job",
            "-p",
            "airlock-controller",
            "-p",
            "airlock-runtime",
            "-p",
            "transponder",
            "-p",
            "mainframe-controller",
            "-p",
            "relay-controller",
        ],
    )?;

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

    // transponder packages to sycophant-transponder:local (name differs from binary).
    let staged = stage(
        repo,
        triple,
        "transponder",
        &format!("transponder-linux-musl-{darch}"),
    )?;
    let archarg = format!("TARGETARCH={darch}");
    docker_build(
        repo,
        &[
            "-f",
            "build/Dockerfile",
            "--build-arg",
            "BINARY=transponder",
            "--build-arg",
            &archarg,
            "-t",
            "sycophant-transponder:local",
            ".",
        ],
    )?;
    let _ = fs::remove_file(&staged);

    // Chambers: the airlock-runtime binary staged into each chamber context.
    for img in CHAMBER_IMAGES {
        let ctx = chamber_context(img);
        let staged = stage(
            repo,
            triple,
            "airlock-runtime",
            &format!("{ctx}/airlock-runtime-linux-{darch}"),
        )?;
        let archarg = format!("TARGETARCH={darch}");
        let tag = format!("{img}:local");
        if img == "airlock-ssh-credentials" {
            docker_build(repo, &["--build-arg", &archarg, ctx, "-t", &tag])?;
        } else {
            let dockerfile = format!("{ctx}/Dockerfile");
            docker_build(
                repo,
                &["--build-arg", &archarg, "-f", &dockerfile, ctx, "-t", &tag],
            )?;
        }
        let _ = fs::remove_file(&staged);
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
    // Chamber images go through the local registry so airlock-controller can read
    // their OCI manifests for tool discovery.
    for img in CHAMBER_IMAGES {
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
    fn chamber_contexts_map_each_image() {
        // Mutant swapping any target dir is caught here.
        assert_eq!(chamber_context("airlock-chamber"), "images/airlock-chamber");
        assert_eq!(chamber_context("airlock-git"), "images/git");
        assert_eq!(
            chamber_context("airlock-ssh-credentials"),
            "examples/chambers/ssh-credentials"
        );
    }

    #[test]
    fn transponder_is_not_a_plain_controller() {
        // It packages to sycophant-transponder:local, so it must be handled
        // separately, never in the CONTROLLER_BINS <name>:local loop.
        assert!(!CONTROLLER_BINS.contains(&"transponder"));
    }

    #[test]
    fn registry_only_chambers_are_not_imported() {
        // airlock-chamber + ssh-credentials reach the cluster ONLY via the
        // registry, never k3d image import.
        for img in ["airlock-chamber", "airlock-ssh-credentials"] {
            assert!(!IMPORT_IMAGES.contains(&format!("{img}:local").as_str()));
        }
        // airlock-git is the exception: the workspace-init Job runs it node-local
        // (pullPolicy=Never), so it is BOTH registry-pushed and k3d-imported.
        assert!(IMPORT_IMAGES.contains(&"airlock-git:local"));
        assert!(CHAMBER_IMAGES.contains(&"airlock-git"));
    }
}
