//! Acceptance tests for the shared reader: `syco toolset manifest <image-ref>`
//! reads a BUILT image's baked schema and emits the capability-manifest content.
//!
//! These exercise the reader end to end against real images, so they need
//! `docker` and prebuilt toolset images. They SKIP LOUDLY when those are absent;
//! run them on the docker-enabled e2e runner that already builds the images.
//! Point the tests at the images via env vars:
//!
//!   SYCO_TEST_TOOLSET_IMAGE       a built toolset image carrying the design-A
//!                                 baked schema (the stdlib `toolset` image;
//!                                 Shell/Read/Write/Edit/Search).
//!   SYCO_TEST_TOOLSET_BASE_IMAGE  the toolset-base image, which carries NO baked
//!                                 schema (the fail-closed subject + the build
//!                                 base for the divergence build).
//!
//! The assertions are on observable behavior, not the subcommand spelling: if the
//! command name changes, update `run_reader` only.

use std::path::PathBuf;
use std::process::{Command, Output};

const SYCO: &str = env!("CARGO_BIN_EXE_syco");

/// The five stdlib builtins the `toolset` image implements.
const STDLIB_TOOLS: &[&str] = &["Shell", "Read", "Write", "Edit", "Search"];

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/cli
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn docker_available() -> bool {
    Command::new("docker")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn image_present(reference: &str) -> bool {
    Command::new("docker")
        .args(["image", "inspect", reference])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run the shared reader against an image reference.
fn run_reader(image_ref: &str) -> Output {
    Command::new(SYCO)
        .args(["toolset", "manifest", image_ref])
        .output()
        .expect("failed to spawn syco")
}

/// Skip helper: returns Some(image_ref) when the env var is set and (docker is
/// up and the image exists), else prints a loud skip and returns None.
fn require_image(env_var: &str, test: &str) -> Option<String> {
    if !docker_available() {
        eprintln!("SKIPPED {test}: docker not available; run on a docker-enabled runner.");
        return None;
    }
    match std::env::var(env_var) {
        Ok(r) if image_present(&r) => Some(r),
        Ok(r) => {
            eprintln!("SKIPPED {test}: {env_var}={r} is not a present docker image.");
            None
        }
        Err(_) => {
            eprintln!("SKIPPED {test}: set {env_var} to a built toolset image.");
            None
        }
    }
}

/// The reader emits a manifest whose tools carry the FULL baked schema: every
/// stdlib tool name, a non-empty `parameters_json` (the JSON Schema the model
/// reads), the owning `toolset`, and `args`. Breaks if the reader emits an empty
/// manifest, or the image bakes only `args` and leaves `parameters_json` as "{}".
#[test]
fn reader_emits_full_manifest_for_schema_bearing_image() {
    let Some(image) = require_image(
        "SYCO_TEST_TOOLSET_IMAGE",
        "reader_emits_full_manifest_for_schema_bearing_image",
    ) else {
        return;
    };

    let out = run_reader(&image);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "reader failed on a schema-bearing image ({image}):\nstderr: {stderr}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "reader emitted an empty manifest for a schema-bearing image"
    );

    for tool in STDLIB_TOOLS {
        assert!(
            stdout.contains(tool),
            "manifest is missing stdlib tool {tool}:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("parameters_json"),
        "manifest tools carry no parameters_json:\n{stdout}"
    );
    // The parameters half must be a real schema, not an empty object.
    assert!(
        !stdout.contains("parameters_json: '{}'") && !stdout.contains("parameters_json: \"{}\""),
        "manifest parameters_json is empty ({{}}); the full JSON Schema was not baked:\n{stdout}"
    );
    assert!(
        stdout.contains("args"),
        "manifest tools carry no args:\n{stdout}"
    );
    assert!(
        stdout.contains("toolset"),
        "manifest tools name no owning toolset:\n{stdout}"
    );
}

/// Fail closed: an image whose carrier has no baked schema makes the reader
/// error with a non-zero exit and NO output -- never an empty manifest that would
/// silently render every tool "unknown". Breaks if the reader swallows the
/// missing schema and emits empty-but-successful output.
///
/// The success sibling above guards that the subcommand actually works, so this
/// is not vacuously green from an absent subcommand.
#[test]
fn reader_fails_closed_on_image_without_baked_schema() {
    let Some(base) = require_image(
        "SYCO_TEST_TOOLSET_BASE_IMAGE",
        "reader_fails_closed_on_image_without_baked_schema",
    ) else {
        return;
    };

    let out = run_reader(&base);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "reader exited 0 on an image with no baked schema; it must fail closed.\n\
         stdout: {stdout}"
    );
    assert!(
        stdout.trim().is_empty(),
        "reader emitted output for an image with no baked schema; a partial/empty \
         manifest must never be produced:\n{stdout}"
    );
    assert!(
        !stderr.trim().is_empty(),
        "reader failed silently; fail-closed must explain the missing schema"
    );
}

/// (Property: manifest-matches-image) The manifest describes the tools the BOUND
/// IMAGE implements, not the repo's tools.yaml. Build a variant image from the
/// real leaf Dockerfile but with one tool renamed in its tools.yaml; the reader's
/// manifest for that image must show the renamed tool (the image's baked schema),
/// while the pristine image's manifest must not. Breaks if the reader derives the
/// manifest from the checked-in file rather than the image it is handed.
#[test]
fn manifest_matches_image_not_repo_file() {
    let test = "manifest_matches_image_not_repo_file";
    let Some(base) = require_image("SYCO_TEST_TOOLSET_BASE_IMAGE", test) else {
        return;
    };
    let Some(pristine) = require_image("SYCO_TEST_TOOLSET_IMAGE", test) else {
        return;
    };

    let src_dir = repo_root().join("images").join("toolset");
    let tools_yaml = src_dir.join("tools.yaml");
    if !tools_yaml.exists() {
        eprintln!(
            "SKIPPED {test}: {} absent (the design-A single source). \
             Runs once the toolset carries tools.yaml.",
            tools_yaml.display()
        );
        return;
    }

    // Assemble a build context: the real Dockerfile + image files, with tools.yaml
    // diverged by renaming the `Shell` tool. Renaming the name token (not a
    // description) keeps this shape-agnostic across the file's exact YAML layout.
    let ctx = std::env::temp_dir().join(format!("syco-divergent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ctx);
    std::fs::create_dir_all(&ctx).expect("mk temp ctx");
    for entry in std::fs::read_dir(&src_dir).expect("read images/toolset") {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), ctx.join(entry.file_name())).unwrap();
        }
    }
    let original = std::fs::read_to_string(&tools_yaml).unwrap();
    assert!(
        original.contains("Shell"),
        "stdlib tools.yaml has no Shell tool to diverge; adjust the rename target"
    );
    let diverged = original.replace("Shell", "GhostDivergent");
    std::fs::write(ctx.join("tools.yaml"), diverged).unwrap();

    let tag = "syco-test-divergent-manifest:local";
    let build = Command::new("docker")
        .args(["build", "-f"])
        .arg(ctx.join("Dockerfile"))
        .args(["--build-arg", &format!("BASE_IMAGE={base}"), "-t", tag])
        .arg(&ctx)
        .output()
        .expect("docker build");
    assert!(
        build.status.success(),
        "failed to build divergent image:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let diverged_manifest = run_reader(tag);
    let diverged_out = String::from_utf8_lossy(&diverged_manifest.stdout).to_string();

    let pristine_manifest = run_reader(&pristine);
    let pristine_out = String::from_utf8_lossy(&pristine_manifest.stdout).to_string();

    // best-effort cleanup
    let _ = Command::new("docker").args(["rmi", "-f", tag]).output();
    let _ = std::fs::remove_dir_all(&ctx);

    assert!(
        diverged_manifest.status.success(),
        "reader failed on the divergent image:\n{}",
        String::from_utf8_lossy(&diverged_manifest.stderr)
    );
    assert!(
        diverged_out.contains("GhostDivergent"),
        "the divergent image's baked schema (GhostDivergent) is absent from its \
         manifest; the reader is not reading the image's schema:\n{diverged_out}"
    );
    assert!(
        !pristine_out.contains("GhostDivergent"),
        "the pristine image's manifest names a tool only the divergent image bakes; \
         the manifest is not derived from the image it describes"
    );
}
