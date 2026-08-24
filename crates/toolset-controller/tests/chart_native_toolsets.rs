//! Acceptance tests for chart-native toolsets: the runtime-logic criteria that
//! cannot be observed from the repo/chart surface asserted in
//! `tests/acceptance/toolset-pod-collapse.sh`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Container, EnvVar};
use kube::client::Body as KubeBody;
use tonic::{Code, Request, Status};

use toolset_controller::audience_layer::RequiredAudience;
use toolset_controller::config::{PromptProfile, Scalar, ToolsetEntry};
use toolset_controller::grpc::{ControllerService, VerifierPair};
use toolset_controller::job::{build_prompt_job, build_tool_job};
use toolset_controller::keepalive::TOOL_KEEPALIVE_IDLE_SECONDS;
use toolset_controller::state::{ControllerState, PromptConfig, WorkspaceBindings};
use toolset_proto::toolset_controller_server::ToolsetController;
use toolset_proto::TurnRequest;

use shared::auth::TokenVerifier;
use shared::scheduling::SchedulingConfig;

const WORKSPACE: &str = "ws";
const NAMESPACE: &str = "test-ns";
const CONTROLLER_ADDR: &str = "http://toolset-ctrl:9090";
const TOOL_IMAGE: &str = "ghcr.io/sycophant/stdlib@sha256:tool";
const PROMPT_IMAGE: &str = "ghcr.io/sycophant/prompt@sha256:prompt";
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

/// Same guarantee on the prompt path, where the profile carries the LLM
/// settings the retired Provider/Model resources used to project.
#[test]
fn prompt_job_forwards_profile_settings_but_not_image_or_keepalive() {
    let p = prompt_section_profile("gpt-x-2026");

    let job = build_prompt_job(
        "fast",
        &p,
        CONTROLLER_ADDR,
        NAMESPACE,
        "sess1",
        WORKSPACE,
        &SchedulingConfig::default(),
    );

    assert_eq!(
        container(&job).image.as_deref(),
        Some(PROMPT_IMAGE),
        "prompt job image must come from the prompt profile"
    );
    assert_no_per_toolset_attr_env(&job, PROMPT_IMAGE);

    // Format, model, and base URL arrive as forwarded profile env.
    assert_eq!(plain_env(&job, "TOOLSET_FORMAT").as_deref(), Some("openai"));
    assert_eq!(
        plain_env(&job, "TOOLSET_MODEL").as_deref(),
        Some("gpt-x-2026"),
        "TOOLSET_MODEL must be the profile's forwarded value, not the profile key"
    );
    assert_eq!(
        plain_env(&job, "TOOLSET_BASE_URL").as_deref(),
        Some("https://api.example.test/v1"),
        "base URL must be forwarded from the profile, not derived from a format"
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

// =========================================================================
// No operator-sourced LLM params on the prompt Job
// =========================================================================

/// Fails if the `TOOLSET_PARAMS` env write is rehomed onto a
/// profile instead of deleted.
#[test]
fn prompt_job_carries_no_operator_params_env() {
    let p = prompt_section_profile("gpt-x-2026");

    let job = build_prompt_job(
        "fast",
        &p,
        CONTROLLER_ADDR,
        NAMESPACE,
        "sess1",
        WORKSPACE,
        &SchedulingConfig::default(),
    );

    assert!(
        !env_map(&job).contains_key("TOOLSET_PARAMS"),
        "operator-sourced LLM params are deleted, not rehomed"
    );
}

// =========================================================================
// The toolset label carries the profile key
// =========================================================================

/// Fails if the label stamp reverts to the toolset name. The
/// chart renders one CNP per PROFILE and selects on this label, so a
/// toolset-name stamp would put every model profile under one egress policy.
#[test]
fn prompt_job_toolset_label_carries_the_profile_key_not_the_toolset_name() {
    let p = prompt_section_profile("claude-x");

    let job = build_prompt_job(
        "smart",
        &p,
        CONTROLLER_ADDR,
        NAMESPACE,
        "sess1",
        WORKSPACE,
        &SchedulingConfig::default(),
    );

    let labels = pod_labels(&job);
    assert_eq!(
        labels.get("sycophant.md/toolset").map(String::as_str),
        Some("smart"),
        "the CNP selector label must carry the profile key, not a toolset name"
    );

    let job_labels = job.metadata.labels.clone().unwrap_or_default();
    assert_eq!(
        job_labels.get("sycophant.md/toolset").map(String::as_str),
        Some("smart")
    );
}

// =========================================================================
// An absent profile key is rejected, never defaulted
// =========================================================================

struct FixedWorkspaceVerifier(String);

#[tonic::async_trait]
impl TokenVerifier for FixedWorkspaceVerifier {
    async fn verify_token(&self, _token: &str) -> Result<String, Status> {
        Ok(self.0.clone())
    }
}

/// Stands in for the API server: echoes any POST back as a 201 so a spawn that
/// SHOULD NOT have happened still completes, and the test observes an `Ok`
/// instead of a hang.
fn mock_kube_client() -> kube::Client {
    let posted = Arc::new(Mutex::new(0usize));
    let svc = tower::service_fn(move |req: http::Request<KubeBody>| {
        let posted = posted.clone();
        async move {
            let (parts, body) = req.into_parts();
            let bytes = body.collect().await.expect("collect body").to_bytes();
            if parts.method == http::Method::POST {
                *posted.lock().unwrap() += 1;
            }
            let resp = http::Response::builder()
                .status(201)
                .header("content-type", "application/json")
                .body(KubeBody::from(bytes.to_vec()))
                .expect("build response");
            Ok::<_, std::convert::Infallible>(resp)
        }
    });
    kube::Client::new(svc, NAMESPACE)
}

fn harness_req<T>(inner: T) -> Request<T> {
    let mut req = Request::new(inner);
    req.metadata_mut()
        .insert("authorization", "Bearer test".parse().unwrap());
    req.extensions_mut().insert(RequiredAudience::Harness);
    req
}

/// A profile of the prompt configuration section. The prompt toolset is the
/// hardcoded turn server: it is not a name-keyed entry of the toolsets map and
/// it appears in no workspace's toolset bindings.
fn prompt_section_profile(model: &str) -> PromptProfile {
    PromptProfile {
        image: PROMPT_IMAGE.to_string(),
        format: "openai".to_string(),
        model: model.to_string(),
        base_url: "https://api.example.test/v1".to_string(),
        secret: "provider-api-key".to_string(),
        egress: vec![],
    }
}

fn turn_service(profiles: &[(&str, PromptProfile)]) -> ControllerService {
    let state = ControllerState::new(
        Some(mock_kube_client()),
        NAMESPACE.into(),
        CONTROLLER_ADDR.into(),
        SchedulingConfig::default(),
    );
    let prompt: HashMap<String, PromptProfile> = profiles
        .iter()
        .map(|(k, p)| (k.to_string(), p.clone()))
        .collect();
    ControllerService::new(
        state,
        Some(VerifierPair {
            harness: Arc::new(FixedWorkspaceVerifier(WORKSPACE.into())),
            tool_job: Arc::new(FixedWorkspaceVerifier(WORKSPACE.into())),
        }),
        // No workspace binds the prompt toolset: the controller resolves it
        // without a name lookup, so no binding can gate it.
        WorkspaceBindings::empty(),
        PromptConfig::from_map(prompt),
    )
}

/// Materiality: fails if profile resolution falls back to a default, to the
/// alphabetic-first profile, or to any other registered profile when the
/// requested `model` value is absent. Two profiles are registered precisely so
/// a fallback has somewhere to land — a fallback returns `Ok` (or spawns),
/// never `FailedPrecondition`.
#[tokio::test]
async fn turn_rejects_a_model_absent_from_the_prompt_profile_map() {
    let svc = turn_service(&[
        ("aardvark", prompt_section_profile("a-model")),
        ("smart", prompt_section_profile("s-model")),
    ]);

    let req = TurnRequest {
        system: None,
        tools: vec![],
        messages: vec![],
        model: Some("not-a-registered-profile".into()),
        reply_channel: None,
        role: None,
        correlation_id: None,
        conversation_id: "conv-1".into(),
    };

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        svc.turn(harness_req(req)),
    )
    .await
    .expect("turn must reject promptly, not spawn and wait for a prompt job");

    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("an absent profile key must be refused, not defaulted to a fallback"),
    };
    assert_eq!(
        err.code(),
        Code::FailedPrecondition,
        "refusal must be FailedPrecondition, got {err:?}"
    );
    assert!(
        err.message().contains("not-a-registered-profile"),
        "the refusal must name the rejected profile key, got: {}",
        err.message()
    );
}

/// The sibling of the above for an ABSENT `model`, which is a distinct branch:
/// a wrongly-named model is refused for missing the profile map, whereas an
/// absent one is refused before the map is consulted at all. Registering two
/// profiles again gives a fallback somewhere to land.
///
/// Materiality: fails if an absent `model` is resolved to a reserved `default`
/// profile, to the alphabetic-first profile, or to any other registered
/// profile. This branch is the one a harness hits when it composes a turn
/// without threading a model through.
#[tokio::test]
async fn turn_rejects_an_absent_model_rather_than_defaulting() {
    let svc = turn_service(&[
        ("aardvark", prompt_section_profile("a-model")),
        ("smart", prompt_section_profile("s-model")),
    ]);

    let req = TurnRequest {
        system: None,
        tools: vec![],
        messages: vec![],
        model: None,
        reply_channel: None,
        role: None,
        correlation_id: None,
        conversation_id: "conv-1".into(),
    };

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        svc.turn(harness_req(req)),
    )
    .await
    .expect("turn must reject promptly, not spawn and wait for a prompt job");

    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("an absent model must be refused, not defaulted to a fallback profile"),
    };
    assert_eq!(
        err.code(),
        Code::FailedPrecondition,
        "refusal must be FailedPrecondition, got {err:?}"
    );
}
