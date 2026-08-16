//! Acceptance test for the net-new discovery Job builder.
//!
//! AC covered:
//!   - "When discovery runs, the system shall perform the registry reach from
//!     an ephemeral discovery Job pod running under the gVisor runtime class"
//!     (the pod carries `app.kubernetes.io/component: tool-job` so Kyverno
//!     stamps `runtimeClassName: gvisor`, and sets NO runtimeClassName itself).
//!   - the reconcile's discovery transport: the Job carries the toolset name,
//!     the target image, and the controller address in env so it can report the
//!     correct toolset's tools back over `ReportDiscoveredTools`, and mounts the
//!     `tool.toolset` tool-job-audience token to authenticate that report.
//!
//! Pinned contract the coder must expose (plan Stage 3, step 5). The plan does
//! not specify the signature, so the tester fixes it here as the coder's input:
//!   toolset_controller::job::build_discovery_job(
//!       toolset_name: &str,
//!       toolset_image: &str,
//!       namespace: &str,
//!       controller_addr: &str,
//!       workspace_name: &str,
//!       scheduling: &shared::scheduling::SchedulingConfig,
//!   ) -> k8s_openapi::api::batch::v1::Job
//!
//! Red-by-missing-symbol: does not compile against the current tree because
//! `build_discovery_job` does not exist yet.
//!
//! Materiality: fails if the builder drops the `tool-job` component label
//! (no gVisor stamp), sets a runtimeClassName itself, omits the discovery
//! discriminator label (so the discovery netpol would select all tool jobs),
//! omits the tool-job-audience token, does not run the `discover` subcommand, or
//! fails to pass the toolset name / image / controller address the report
//! depends on.

use shared::scheduling::SchedulingConfig;
use toolset_controller::job::build_discovery_job;

const TOOLSET: &str = "stdlib";
const TOOLSET_IMAGE: &str = "ghcr.io/test/stdlib:latest";
const NAMESPACE: &str = "test-ns";
const CONTROLLER_ADDR: &str = "http://toolset-ctrl:9090";
const WORKSPACE: &str = "test";

fn discovery_job() -> k8s_openapi::api::batch::v1::Job {
    build_discovery_job(
        TOOLSET,
        TOOLSET_IMAGE,
        NAMESPACE,
        CONTROLLER_ADDR,
        WORKSPACE,
        &SchedulingConfig::default(),
    )
}

fn pod_template(
    job: &k8s_openapi::api::batch::v1::Job,
) -> &k8s_openapi::api::core::v1::PodTemplateSpec {
    &job.spec.as_ref().expect("job spec").template
}

fn pod_spec(job: &k8s_openapi::api::batch::v1::Job) -> &k8s_openapi::api::core::v1::PodSpec {
    pod_template(job).spec.as_ref().expect("pod spec")
}

fn pod_labels(
    job: &k8s_openapi::api::batch::v1::Job,
) -> &std::collections::BTreeMap<String, String> {
    pod_template(job)
        .metadata
        .as_ref()
        .expect("pod template metadata")
        .labels
        .as_ref()
        .expect("pod template labels")
}

#[test]
fn discovery_job_pod_is_gated_as_tool_job_without_runtime_class() {
    let job = discovery_job();
    let labels = pod_labels(&job);
    assert_eq!(
        labels
            .get("app.kubernetes.io/component")
            .map(String::as_str),
        Some("tool-job"),
        "the discovery pod must be a tool-job so Kyverno stamps runtimeClassName: gvisor"
    );
    assert_eq!(
        pod_spec(&job).runtime_class_name,
        None,
        "the builder must NOT set runtimeClassName itself — admission stamps gVisor"
    );
}

#[test]
fn discovery_job_pod_carries_workspace_and_discovery_discriminator_labels() {
    let job = discovery_job();
    let labels = pod_labels(&job);
    assert_eq!(
        labels.get("sycophant.md/workspace").map(String::as_str),
        Some(WORKSPACE),
        "the pod must carry a non-empty workspace label"
    );
    assert_eq!(
        labels.get("sycophant.md/job").map(String::as_str),
        Some("discovery"),
        "the pod must carry the discovery discriminator label so the discovery \
         netpol selects it alone, not every tool job"
    );
}

#[test]
fn discovery_job_runs_the_discover_subcommand() {
    let job = discovery_job();
    let container = &pod_spec(&job).containers[0];
    assert_eq!(
        container.command, None,
        "the discovery pod must not override the controller image entrypoint (/app)"
    );
    assert_eq!(
        container.args.as_deref(),
        Some(&["discover".to_string()][..]),
        "the discovery pod must run the /app entrypoint under the `discover` subcommand"
    );
}

#[test]
fn discovery_job_mounts_the_tool_job_audience_token() {
    let job = discovery_job();
    let volumes = pod_spec(&job).volumes.as_ref().expect("pod volumes");
    let has_tool_job_token = volumes.iter().any(|v| {
        v.projected
            .as_ref()
            .and_then(|p| p.sources.as_ref())
            .map(|sources| {
                sources.iter().any(|s| {
                    s.service_account_token
                        .as_ref()
                        .and_then(|sat| sat.audience.as_deref())
                        == Some(shared::auth::TOOL_TOOLSET_AUDIENCE)
                })
            })
            .unwrap_or(false)
    });
    assert!(
        has_tool_job_token,
        "the discovery pod must mount a projected token with the tool.toolset \
         tool-job audience so it can authenticate its ReportDiscoveredTools call"
    );
}

#[test]
fn discovery_job_env_carries_toolset_name_image_and_controller_addr() {
    let job = discovery_job();
    let env: std::collections::BTreeMap<&str, &str> = pod_spec(&job).containers[0]
        .env
        .as_ref()
        .expect("container env")
        .iter()
        .filter_map(|e| e.value.as_deref().map(|v| (e.name.as_str(), v)))
        .collect();
    assert_eq!(
        env.get("TOOLSET_TOOLSET_NAME"),
        Some(&TOOLSET),
        "the Job must know which toolset it discovers so the report keys the right toolset"
    );
    assert_eq!(
        env.get("TOOLSET_IMAGE"),
        Some(&TOOLSET_IMAGE),
        "the Job must know which image to read the tool label from"
    );
    assert_eq!(
        env.get("TOOLSET_CONTROLLER_ADDR"),
        Some(&CONTROLLER_ADDR),
        "the Job must know where to report the discovered tools"
    );
}
