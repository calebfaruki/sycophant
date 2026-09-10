//! Chart-native toolsets: the runtime resolution that cannot be observed from
//! the rendered chart alone.

use std::collections::BTreeMap;

use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Container, EnvVar};

use toolset_controller::config::{Scalar, ToolsetEntry};
use toolset_controller::job::build_tool_job;
use toolset_controller::keepalive::TOOL_KEEPALIVE_IDLE_SECONDS;

use shared::scheduling::SchedulingConfig;

const WORKSPACE: &str = "ws";
const NAMESPACE: &str = "test-ns";
const CONTROLLER_ADDR: &str = "http://toolset-ctrl:9090";
const TOOL_IMAGE: &str = "ghcr.io/sycophant/stdlib@sha256:tool";
const CALL_ID: &str = "abcdef12-0000-0000-0000-000000000000";

// =========================================================================
// Fixtures
// =========================================================================

fn yaml(s: &str) -> Scalar {
    Scalar::String(s.to_string())
}

fn entry(image: &str, keepalive: bool, env: &[(&str, &str)]) -> ToolsetEntry {
    ToolsetEntry {
        image: Some(image.to_string()),
        keepalive,
        env: env.iter().map(|(k, v)| (k.to_string(), yaml(v))).collect(),
        ..Default::default()
    }
}

// =========================================================================
// Job introspection helpers
// =========================================================================

fn container(job: &Job) -> &Container {
    job.spec
        .as_ref()
        .expect("Job.spec")
        .template
        .spec
        .as_ref()
        .expect("PodSpec")
        .containers
        .first()
        .expect("one container")
}

fn env_map(job: &Job) -> BTreeMap<String, EnvVar> {
    container(job)
        .env
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|e| (e.name.clone(), e))
        .collect()
}

fn plain_env(job: &Job, name: &str) -> Option<String> {
    env_map(job).get(name).and_then(|e| e.value.clone())
}

fn restart_policy(job: &Job) -> String {
    job.spec
        .as_ref()
        .expect("Job.spec")
        .template
        .spec
        .as_ref()
        .expect("PodSpec")
        .restart_policy
        .clone()
        .expect("restartPolicy")
}

/// Every env var name a caller could confuse with the per-toolset attributes.
/// Forwarding uses the `env` key VERBATIM, so `image`/`keepalive` leaking
/// through the forward loop would appear under exactly these names.
fn assert_no_per_toolset_attr_env(job: &Job, image: &str) {
    let env = env_map(job);
    for name in env.keys() {
        assert_ne!(
            name.to_ascii_lowercase(),
            "image",
            "per-toolset `image` must never be forwarded as an env var (found {name})"
        );
        assert_ne!(
            name.to_ascii_lowercase(),
            "keepalive",
            "per-toolset `keepalive` must never be forwarded as an env var (found {name})"
        );
    }
    for (name, var) in &env {
        assert_ne!(
            var.value.as_deref(),
            Some(image),
            "env `{name}` carries the toolset image; `image` selects the pod, it is not tool-job env"
        );
    }
}

// =========================================================================
// Image and keepalive are per-toolset, never forwarded
// =========================================================================

/// Fails if the forward loop iterates entry attributes instead of
/// the explicit `env` map (leaking `image`/`keepalive` into tool-job env), or if
/// the container image stops coming from `entry.image`.
#[test]
fn tool_job_reads_image_and_keepalive_from_entry_and_forwards_neither() {
    let e = entry(TOOL_IMAGE, true, &[("NOTION_API_VERSION", "2022-06-28")]);

    let job = build_tool_job(
        "Search",
        "notion",
        &e,
        CALL_ID,
        NAMESPACE,
        CONTROLLER_ADDR,
        WORKSPACE,
        "pvc-ws",
        &SchedulingConfig::default(),
        None,
    );

    // The entry's two attributes are READ: image selects the pod, keepalive
    // sets the restart policy.
    assert_eq!(
        container(&job).image.as_deref(),
        Some(TOOL_IMAGE),
        "tool job image must come from the per-toolset entry"
    );
    assert_eq!(
        restart_policy(&job),
        "OnFailure",
        "entry.keepalive must drive the Job restart policy"
    );

    // Neither is forwarded.
    assert_no_per_toolset_attr_env(&job, TOOL_IMAGE);

    // The `env` key IS forwarded, verbatim name and scalar value.
    assert_eq!(
        plain_env(&job, "NOTION_API_VERSION").as_deref(),
        Some("2022-06-28"),
        "an `env` key must be forwarded verbatim as an env var"
    );
}

// =========================================================================
// Keepalive keeps the tool job warm
// =========================================================================

/// Fails if keepalive stops reaching the restart policy or the
/// tool job's explicit `TOOLSET_KEEPALIVE` signal, or if the idle-reap window
/// collapses to zero (which reaps a warm pod immediately, reinstating
/// cold-start on every call).
#[test]
fn keepalive_entry_keeps_the_tool_job_warm() {
    let warm = entry(TOOL_IMAGE, true, &[]);
    let cold = entry(TOOL_IMAGE, false, &[]);

    let build = |e: &ToolsetEntry| {
        build_tool_job(
            "Search",
            "stdlib",
            e,
            CALL_ID,
            NAMESPACE,
            CONTROLLER_ADDR,
            WORKSPACE,
            "pvc-ws",
            &SchedulingConfig::default(),
            None,
        )
    };

    let warm_job = build(&warm);
    assert_eq!(restart_policy(&warm_job), "OnFailure");
    assert_eq!(
        plain_env(&warm_job, "TOOLSET_KEEPALIVE").as_deref(),
        Some("true"),
        "the controller's explicit keepalive signal to the tool job must survive"
    );

    // The discriminator: a non-keepalive toolset must NOT stay warm, so an
    // unconditional "OnFailure" cannot satisfy this test.
    let cold_job = build(&cold);
    assert_eq!(restart_policy(&cold_job), "Never");
    assert!(
        plain_env(&cold_job, "TOOLSET_KEEPALIVE").is_none(),
        "a non-keepalive toolset must carry no keepalive signal"
    );

    const {
        assert!(
            TOOL_KEEPALIVE_IDLE_SECONDS > 0,
            "idle-reap window must stay non-zero or a warm tool job is reaped at once"
        )
    };
}
