//! The tool job pod a resolved grant produces.
//!
//! Every credential reaches the pod as a file, never as environment: env leaks
//! through `/proc/<pid>/environ`, child process inheritance, and logs. Delivery
//! is stage-and-copy — the controller mounts the grant Secret read-only at a
//! staging path and hands the runtime a `{staging, target}` pair, because
//! neither Secret file mode a direct mount can produce is both owned by the
//! runtime user and unreadable to its group.
//!
//! A credential the pod holds must also be scrubbed out of tool output, gRPC
//! chunks, and log lines, so the resolved grant feeds the scrub registry.

use std::collections::BTreeMap;

use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Container, PodSpec, Volume};

use shared::scheduling::SchedulingConfig;
use toolset_controller::config::ToolsetEntry;
use toolset_controller::job::build_tool_job;
use toolset_controller::state::CapabilityGrant;

const TOOL: &str = "Search";
const TOOLSET: &str = "notion";
const CALL_ID: &str = "abcdef12-0000-0000-0000-000000000000";
const NAMESPACE: &str = "test-ns";
const CONTROLLER_ADDR: &str = "http://toolset-ctrl:9090";
const WORKSPACE: &str = "ws";
const WORKSPACE_PVC: &str = "ws-workspace-data";
const IMAGE: &str = "ghcr.io/sycophant/notion@sha256:trusted";

const GRANT_NAME: &str = "reader";
const GRANT_SECRET: &str = "ws-notion-reader";
const DEFAULT_TARGET: &str = "/run/secrets/grant/credential";
const SSH_TARGET: &str = "/home/agent/.ssh/id_ed25519";

fn entry() -> ToolsetEntry {
    ToolsetEntry {
        image: Some(IMAGE.to_string()),
        ..ToolsetEntry::default()
    }
}

fn grant(path: Option<&str>, egress: Option<&str>) -> CapabilityGrant {
    CapabilityGrant {
        secret: GRANT_SECRET.to_string(),
        path: path.map(str::to_string),
        egress: egress.map(str::to_string),
    }
}

fn job_for(grant: Option<(&str, &CapabilityGrant)>) -> Job {
    build_tool_job(
        TOOL,
        TOOLSET,
        &entry(),
        CALL_ID,
        NAMESPACE,
        CONTROLLER_ADDR,
        WORKSPACE,
        WORKSPACE_PVC,
        &SchedulingConfig::default(),
        grant,
    )
}

// ---- Pod introspection ----

fn pod_spec(job: &Job) -> &PodSpec {
    job.spec
        .as_ref()
        .expect("Job.spec")
        .template
        .spec
        .as_ref()
        .expect("PodSpec")
}

fn container(job: &Job) -> &Container {
    pod_spec(job).containers.first().expect("one container")
}

fn pod_labels(job: &Job) -> BTreeMap<String, String> {
    job.spec
        .as_ref()
        .expect("Job.spec")
        .template
        .metadata
        .as_ref()
        .expect("pod metadata")
        .labels
        .clone()
        .unwrap_or_default()
}

fn env_value(job: &Job, name: &str) -> Option<String> {
    container(job)
        .env
        .as_ref()?
        .iter()
        .find(|e| e.name == name)
        .and_then(|e| e.value.clone())
}

fn secret_volumes(job: &Job) -> Vec<&Volume> {
    pod_spec(job)
        .volumes
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|v| v.secret.is_some())
        .collect()
}

/// The one `{staging, target}` pair the runtime copies at startup.
fn credential_pair(job: &Job) -> (String, String) {
    let raw = env_value(job, "TOOLSET_CREDENTIAL_MAP")
        .expect("a resolved grant hands the runtime a staging-to-target pair");
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the credential map is a JSON array");
    assert_eq!(
        entries.len(),
        1,
        "a resolved grant delivers exactly one credential, got: {entries:?}"
    );
    (
        entries[0]["staging"]
            .as_str()
            .expect("staging path")
            .to_string(),
        entries[0]["target"]
            .as_str()
            .expect("target path")
            .to_string(),
    )
}

// ---- Labels ----

/// The per-grant network policy selects on the grant label together with the
/// workspace and toolset labels, and the baseline floor, the runtimeClass
/// mutation, and the pod policy all key on the component label. All of them
/// must be present from the pod's first instant.
///
/// Breaks if the grant label is dropped, sourced from the Secret name rather
/// than the bound grant name, or written over one of the four existing labels.
#[test]
fn a_resolved_grant_adds_its_label_and_leaves_the_existing_pod_labels_intact() {
    let g = grant(None, Some("notion.com"));
    let labels = pod_labels(&job_for(Some((GRANT_NAME, &g))));

    assert_eq!(
        labels.get("sycophant.md/grant").map(String::as_str),
        Some(GRANT_NAME),
        "the pod carries the grant name as stored in the binding"
    );
    assert_eq!(
        labels
            .get("app.kubernetes.io/component")
            .map(String::as_str),
        Some("tool-job")
    );
    assert_eq!(
        labels.get("app.kubernetes.io/part-of").map(String::as_str),
        Some("sycophant")
    );
    assert_eq!(
        labels.get("sycophant.md/toolset").map(String::as_str),
        Some(TOOLSET)
    );
    assert_eq!(
        labels.get("sycophant.md/workspace").map(String::as_str),
        Some(WORKSPACE)
    );
}

/// Breaks if the grant label is stamped unconditionally, which would make every
/// grantless tool job selectable by some per-grant policy.
#[test]
fn a_grantless_tool_job_carries_no_grant_label() {
    assert!(
        !pod_labels(&job_for(None)).contains_key("sycophant.md/grant"),
        "no grant resolved means no grant label"
    );
}

// ---- Where the credential lands ----

/// An API key has no native home, so it lands at the convention path.
///
/// Breaks if the default target changes, or if an absent `path` is treated as
/// "mount at the staging path and stop".
#[test]
fn a_grant_declaring_no_path_targets_the_convention_credential_file() {
    let g = grant(None, Some("notion.com"));
    let job = job_for(Some((GRANT_NAME, &g)));

    let (_, target) = credential_pair(&job);
    assert_eq!(
        target, DEFAULT_TARGET,
        "a grant with no `path` lands at the convention target"
    );
}

/// A credential whose consumer dictates its location — an ssh key, a kubeconfig
/// — sets `path` and the file lands there.
///
/// Breaks if `path` is ignored, or if the convention default is applied even
/// when the grant names a target.
#[test]
fn a_grant_declaring_a_path_targets_that_path() {
    let g = grant(Some(SSH_TARGET), None);
    let job = job_for(Some((GRANT_NAME, &g)));

    let (_, target) = credential_pair(&job);
    assert_eq!(
        target, SSH_TARGET,
        "a grant with a `path` lands where its consumer expects it"
    );
}

/// The staging mount must be somewhere other than the target: the runtime copies
/// staging to target and chmods the copy, and a Secret mount at the target would
/// leave a file the runtime user either cannot read or shares with its group.
///
/// Breaks if the Secret is mounted directly at the target, if the staging mount
/// loses `readOnly`, or if the mount stops naming the Secret volume.
#[test]
fn the_grant_secret_stages_read_only_at_a_path_distinct_from_its_target() {
    let g = grant(None, Some("notion.com"));
    let job = job_for(Some((GRANT_NAME, &g)));

    let (staging, target) = credential_pair(&job);
    assert_ne!(
        staging, target,
        "the credential is copied from staging to target, so the two must differ"
    );

    let volume = secret_volumes(&job)
        .first()
        .copied()
        .expect("the grant Secret is mounted")
        .clone();
    let mount = container(&job)
        .volume_mounts
        .as_ref()
        .expect("volume mounts")
        .iter()
        .find(|m| m.name == volume.name)
        .expect("the grant Secret volume is mounted into the runtime container")
        .clone();
    assert_eq!(
        mount.mount_path, staging,
        "the Secret mounts at the staging path the runtime is told to copy from"
    );
    assert_eq!(
        mount.read_only,
        Some(true),
        "the staged credential must be read-only"
    );
}

/// The controller cannot enumerate a Secret's keys without reading it, so the
/// grant Secret carries its value under a data key equal to the Secret's own
/// name and exactly that key is projected.
///
/// Breaks if the whole Secret is mounted as a directory, or if the projected key
/// stops being the Secret name.
#[test]
fn the_grant_secret_projects_the_single_data_key_named_for_the_secret() {
    let g = grant(None, Some("notion.com"));
    let job = job_for(Some((GRANT_NAME, &g)));

    let volumes = secret_volumes(&job);
    let source = volumes
        .first()
        .expect("the grant Secret is mounted")
        .secret
        .as_ref()
        .expect("Secret volume source");
    assert_eq!(
        source.secret_name.as_deref(),
        Some(GRANT_SECRET),
        "the mounted Secret is the one the grant names"
    );
    let items = source
        .items
        .as_ref()
        .expect("one key to one file, never the whole Secret as a directory");
    assert_eq!(items.len(), 1, "exactly one data key is projected");
    assert_eq!(
        items[0].key, GRANT_SECRET,
        "the data key equals the Secret's own name"
    );
}

// ---- One credential, never in the environment ----

/// Breaks if the grant Secret is exposed through a `secretKeyRef`, or if the
/// Secret name is interpolated into a plain env value.
#[test]
fn a_grants_secret_is_never_exposed_through_the_environment() {
    let g = grant(None, Some("notion.com"));
    let job = job_for(Some((GRANT_NAME, &g)));

    let env = container(&job).env.clone().unwrap_or_default();
    assert!(
        env.iter().all(|e| e
            .value_from
            .as_ref()
            .and_then(|s| s.secret_key_ref.as_ref())
            .is_none()),
        "no tool-job env var may resolve from a Secret, got: {env:?}"
    );
}

/// A hijacked tool job must hold one credential that works against one
/// destination.
///
/// Breaks if a second Secret-backed volume is emitted from any source.
#[test]
fn a_tool_job_pod_carries_at_most_one_secret_backed_volume() {
    let g = grant(Some(SSH_TARGET), None);
    let job = job_for(Some((GRANT_NAME, &g)));

    assert_eq!(
        secret_volumes(&job).len(),
        1,
        "a resolved grant is the pod's only credential"
    );
}

/// Breaks if credential material is emitted on the grantless path — a Secret
/// volume, a `secretKeyRef`, a staging pair, or a scrub registry with no
/// credential to scrub.
#[test]
fn a_grantless_tool_job_carries_no_credential_material() {
    let job = job_for(None);

    assert!(
        secret_volumes(&job).is_empty(),
        "no grant means no Secret-backed volume"
    );
    assert!(
        container(&job)
            .env
            .clone()
            .unwrap_or_default()
            .iter()
            .all(|e| e
                .value_from
                .as_ref()
                .and_then(|s| s.secret_key_ref.as_ref())
                .is_none()),
        "no grant means no secretKeyRef env var"
    );
    assert_eq!(
        env_value(&job, "TOOLSET_CREDENTIAL_MAP"),
        None,
        "no grant means nothing to stage"
    );
    assert_eq!(
        env_value(&job, "TOOLSET_SCRUB_SECRETS"),
        None,
        "no grant means nothing to scrub"
    );
}

// ---- The scrubber follows the credential ----

/// The runtime reads each registered value from its file and redacts it from
/// tool output, gRPC chunks, and log lines. The registry must therefore name
/// the file the credential ends up in, not the staging copy the tool never
/// reads.
///
/// Breaks if the resolved grant stops feeding the scrub registry, or if the
/// registry records the staging path instead of the target.
#[test]
fn a_resolved_grant_registers_its_secret_and_target_for_scrubbing() {
    let g = grant(Some(SSH_TARGET), None);
    let job = job_for(Some((GRANT_NAME, &g)));

    let raw = env_value(&job, "TOOLSET_SCRUB_SECRETS")
        .expect("the pod's one credential must be scrubbed from what leaves the pod");
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the scrub registry is a JSON array");
    assert_eq!(entries.len(), 1, "one credential, one registry entry");
    assert_eq!(
        entries[0]["name"].as_str(),
        Some(GRANT_SECRET),
        "the entry names the Secret the value came from"
    );
    assert_eq!(
        entries[0]["file"].as_str(),
        Some(SSH_TARGET),
        "the entry names the target file the runtime reads the value from"
    );
}

/// Breaks if the default target is applied to the credential map but not to the
/// scrub registry, leaving the convention-path credential unredacted.
#[test]
fn a_grant_with_no_path_registers_the_convention_target_for_scrubbing() {
    let g = grant(None, Some("notion.com"));
    let job = job_for(Some((GRANT_NAME, &g)));

    let raw = env_value(&job, "TOOLSET_SCRUB_SECRETS").expect("scrub registry");
    let entries: Vec<serde_json::Value> = serde_json::from_str(&raw).expect("JSON array");
    assert_eq!(
        entries[0]["file"].as_str(),
        Some(DEFAULT_TARGET),
        "the registry follows the credential to the convention target"
    );
}
