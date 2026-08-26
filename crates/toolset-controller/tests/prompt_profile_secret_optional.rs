//! The prompt Job a profile produces, with and without a provider credential.
//! A `baseUrl` inside the cluster authenticates nobody, so its profile omits
//! `secret` and its pod carries no credential material. A declared `secret`
//! keeps today's delivery unchanged.

use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Container, PodSpec, Volume};

use shared::scheduling::testing::no_scheduling;
use toolset_controller::config::PromptProfile;
use toolset_controller::job::build_prompt_job;

const PROFILE_KEY: &str = "local";
const NAMESPACE: &str = "test-ns";
const CONTROLLER_ADDR: &str = "http://toolset-ctrl:9090";
const SESSION: &str = "sess1";
const WORKSPACE: &str = "ws";

/// Where the prompt image reads its provider credential.
const CREDENTIAL_PATH: &str = "/run/secrets/toolset/api-key";
const SECRET_NAME: &str = "sycophant-llm-openrouter";

const WITHOUT_SECRET: &str = "\
image: prompt-toolset:local
format: openai
model: liquid/lfm2.5-8b-a1b
baseUrl: http://inference-local:8080/v1
";

const WITH_SECRET: &str = "\
image: prompt-toolset:local
format: openai
model: deepseek/deepseek-v4-flash
baseUrl: https://openrouter.ai/api/v1
secret: sycophant-llm-openrouter
";

/// Parsed, not struct-literal: parsing is where an omitted `secret` is accepted
/// or refused, and the chart authors the profile as YAML.
fn profile(yaml: &str) -> PromptProfile {
    serde_yaml::from_str(yaml)
        .unwrap_or_else(|e| panic!("the profile must parse as the controller loads it: {e}"))
}

fn job_for(yaml: &str) -> Job {
    build_prompt_job(
        PROFILE_KEY,
        &profile(yaml),
        CONTROLLER_ADDR,
        NAMESPACE,
        SESSION,
        WORKSPACE,
        &no_scheduling(),
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

fn secret_volumes(job: &Job) -> Vec<&Volume> {
    pod_spec(job)
        .volumes
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|v| v.secret.is_some())
        .collect()
}

fn projected_volumes(job: &Job) -> Vec<&Volume> {
    pod_spec(job)
        .volumes
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|v| v.projected.is_some())
        .collect()
}

fn mount_paths(job: &Job) -> Vec<String> {
    container(job)
        .volume_mounts
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|m| m.mount_path.clone())
        .collect()
}

fn env_value(job: &Job, name: &str) -> Option<String> {
    container(job)
        .env
        .as_ref()?
        .iter()
        .find(|e| e.name == name)
        .and_then(|e| e.value.clone())
}

// ---- No secret declared ----

/// Breaks if the Secret mapping is built unconditionally — an empty or absent
/// `secret` then names a Secret that does not exist, and the kubelet blocks the
/// pod from starting on a volume it can never populate.
#[test]
fn a_profile_declaring_no_secret_spawns_a_pod_with_no_credential_volume() {
    let job = job_for(WITHOUT_SECRET);

    assert!(
        secret_volumes(&job).is_empty(),
        "no declared secret means no Secret-backed volume, got: {:?}",
        secret_volumes(&job)
    );
    assert!(
        !mount_paths(&job).iter().any(|p| p == CREDENTIAL_PATH),
        "no declared secret means nothing mounts at the credential path, got: {:?}",
        mount_paths(&job)
    );

    // The discriminator against "the credential branch deleted every volume":
    // the custom-audience token the controller demands on job methods survives.
    assert_eq!(
        projected_volumes(&job).len(),
        1,
        "the projected audience token is unrelated to the credential and must survive"
    );
}

/// Breaks if the scrub registry is fed from a mapping built around an absent
/// secret, which registers an empty value and redacts every byte of tool output.
#[test]
fn a_profile_declaring_no_secret_registers_nothing_to_scrub() {
    assert_eq!(
        env_value(&job_for(WITHOUT_SECRET), "TOOLSET_SCRUB_SECRETS"),
        None,
        "no credential means nothing to redact"
    );
}

// ---- A declared secret keeps today's delivery ----

/// Breaks if making `secret` optional also changes how a declared one is
/// delivered: the volume must still project the single data key named for the
/// Secret and mount read-only at the path the prompt image reads.
#[test]
fn a_profile_declaring_a_secret_still_mounts_it_read_only_at_the_credential_path() {
    let job = job_for(WITH_SECRET);

    let volumes = secret_volumes(&job);
    assert_eq!(volumes.len(), 1, "one declared secret, one volume");
    let source = volumes[0].secret.as_ref().expect("Secret volume source");
    assert_eq!(source.secret_name.as_deref(), Some(SECRET_NAME));

    let mount = container(&job)
        .volume_mounts
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|m| m.name == volumes[0].name)
        .expect("the credential volume is mounted into the prompt container")
        .clone();
    assert_eq!(mount.mount_path, CREDENTIAL_PATH);
    assert_eq!(mount.read_only, Some(true));
}

/// Breaks if the declared secret stops reaching the scrub registry, which is
/// what keeps the provider key out of tool output, gRPC chunks, and log lines.
#[test]
fn a_profile_declaring_a_secret_registers_it_for_scrubbing() {
    let raw = env_value(&job_for(WITH_SECRET), "TOOLSET_SCRUB_SECRETS")
        .expect("a declared credential must be redacted from what leaves the pod");
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the scrub registry is a JSON array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"].as_str(), Some(SECRET_NAME));
    assert_eq!(entries[0]["file"].as_str(), Some(CREDENTIAL_PATH));
}
