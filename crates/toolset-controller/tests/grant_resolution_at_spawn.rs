//! `__grant` is a framework-reserved tool-call input key naming one grant. The
//! controller compares it by exact closed-set membership against the menu bound
//! for this (workspace, toolset) pair, strips it before input validation, and
//! never interprets it.
//!
//! Absence of `__grant` is not a miss. It is the grantless path: no label, no
//! credential, baseline egress, whether or not the binding entry carries a menu.
//!
//! Driven through `begin_tool_call` against a mock kube API that records every
//! request and captures every POSTed Job, so what is asserted is what the
//! controller actually sends to the apiserver.

use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use kube::client::Body as KubeBody;
use tonic::{Request, Status};

use toolset_controller::audience_layer::RequiredAudience;
use toolset_controller::config::ToolsetEntry;
use toolset_controller::grpc::{ControllerService, VerifierPair};
use toolset_controller::state::{ControllerState, PromptConfig, WorkspaceBindings};
use toolset_proto::toolset_controller_server::ToolsetController;
use toolset_proto::{DiscoveredArgMsg, DiscoveredToolMsg, ReportDiscoveredToolsRequest};

use proto_common::CallToolRequest;
use shared::auth::TokenVerifier;

const WORKSPACE: &str = "ws";
const GRANTED_TOOLSET: &str = "notion";
const BARE_TOOLSET: &str = "stdlib";
const GRANT_NAME: &str = "reader";
const IMAGE: &str = "ghcr.io/sycophant/notion@sha256:trusted";

/// One workspace list mixing a grant-bearing entry and a bare entry, written to
/// disk and loaded through the real bindings loader.
const BINDINGS: &str = "\
ws:
  - name: notion
    grants:
      reader:
        secret: ws-notion-reader
        egress: notion.com
  - stdlib
";

#[derive(Default)]
struct Captured {
    posted_jobs: Vec<serde_json::Value>,
    requests: Vec<(String, String)>,
}

struct FixedWorkspaceVerifier(String);

#[tonic::async_trait]
impl TokenVerifier for FixedWorkspaceVerifier {
    async fn verify_token(&self, _token: &str) -> Result<String, Status> {
        Ok(self.0.clone())
    }
}

/// Stands in for the kube API server: records every request's (method, path),
/// captures the body of every POSTed Job, and echoes the body back as 201 so
/// `Api::create` deserializes a valid `Job`.
fn mock_kube_client(cap: Arc<Mutex<Captured>>) -> kube::Client {
    let svc = tower::service_fn(move |req: http::Request<KubeBody>| {
        let cap = cap.clone();
        async move {
            let (parts, body) = req.into_parts();
            let method = parts.method.to_string();
            let path = parts.uri.path().to_string();
            let bytes = body
                .collect()
                .await
                .expect("mock kube: request body must collect")
                .to_bytes();

            {
                let mut c = cap.lock().unwrap();
                c.requests.push((method, path.clone()));
                if parts.method == http::Method::POST && path.contains("/jobs") {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        c.posted_jobs.push(v);
                    }
                }
            }

            let resp = http::Response::builder()
                .status(201)
                .header("content-type", "application/json")
                .body(KubeBody::from(bytes.to_vec()))
                .expect("mock kube: build response");
            Ok::<_, std::convert::Infallible>(resp)
        }
    });
    kube::Client::new(svc, "test-ns")
}

/// Tests run in parallel threads and the clock is not fine-grained enough to
/// separate them, so the file name carries a counter: two tests sharing one
/// name means one loads the other's fixture.
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn write_temp_bindings() -> String {
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("bindings-{}-{}.yaml", std::process::id(), seq));
    std::fs::write(&path, BINDINGS).expect("write temp bindings file");
    path.to_string_lossy().into_owned()
}

fn bindings() -> WorkspaceBindings {
    let path = write_temp_bindings();
    let loaded = WorkspaceBindings::load(&path);
    let _ = std::fs::remove_file(&path);
    loaded.expect("a workspace list mixing a bare name and a grant-bearing entry must load")
}

fn entry() -> ToolsetEntry {
    ToolsetEntry {
        image: Some(IMAGE.to_string()),
        ..ToolsetEntry::default()
    }
}

fn tool_job_req<T>(inner: T) -> Request<T> {
    let mut req = Request::new(inner);
    req.metadata_mut()
        .insert("authorization", "Bearer test".parse().unwrap());
    req.extensions_mut().insert(RequiredAudience::ToolJob);
    req
}

fn call(tool: &str, input_json: &str) -> Request<CallToolRequest> {
    let mut req = Request::new(CallToolRequest {
        name: tool.into(),
        input_json: input_json.into(),
        conversation_id: String::new(),
    });
    req.metadata_mut()
        .insert("authorization", "Bearer test".parse().unwrap());
    req.extensions_mut().insert(RequiredAudience::Harness);
    req
}

/// A controller wired to `toolset`, with one tool `tool` carrying `args`
/// registered against it, plus the capture handle for the mock apiserver.
async fn controller(
    toolset: &str,
    tool: &str,
    args: Vec<DiscoveredArgMsg>,
) -> (ControllerService, Arc<Mutex<Captured>>) {
    let cap = Arc::new(Mutex::new(Captured::default()));
    let state = ControllerState::new(
        Some(mock_kube_client(cap.clone())),
        "test-ns".into(),
        "http://toolset-ctrl:9090".into(),
        shared::scheduling::SchedulingConfig::default(),
    );
    state.set_toolset(toolset.into(), entry()).await;

    let verifiers = VerifierPair {
        harness: Arc::new(FixedWorkspaceVerifier(WORKSPACE.into())),
        tool_job: Arc::new(FixedWorkspaceVerifier(WORKSPACE.into())),
    };
    let svc = ControllerService::new(state, Some(verifiers), bindings(), PromptConfig::empty());

    svc.report_discovered_tools(tool_job_req(ReportDiscoveredToolsRequest {
        toolset_name: toolset.into(),
        tools: vec![DiscoveredToolMsg {
            name: tool.into(),
            description: "t".into(),
            args,
        }],
    }))
    .await
    .expect("the tool-job discovery report must be accepted");

    (svc, cap)
}

fn grant_label(job: &serde_json::Value) -> Option<String> {
    job.pointer("/spec/template/metadata/labels/sycophant.md~1grant")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn container_env(job: &serde_json::Value) -> Vec<serde_json::Value> {
    job.pointer("/spec/template/spec/containers/0/env")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

// ---- A member value resolves ----

/// Breaks if the resolved grant is not handed to the job builder, if the
/// matched name is sourced from anything but the bound menu key, or if a
/// matching call spawns more than one job.
#[tokio::test]
async fn a_call_naming_a_bound_grant_spawns_one_job_stamped_with_that_grant() {
    let (svc, cap) = controller(GRANTED_TOOLSET, "Search", vec![]).await;

    svc.begin_tool_call(call("Search", r#"{"__grant":"reader"}"#))
        .await
        .expect("a call naming a member of the bound menu must be accepted");

    let c = cap.lock().unwrap();
    assert_eq!(
        c.posted_jobs.len(),
        1,
        "a resolved grant spawns exactly one tool job"
    );
    assert_eq!(
        grant_label(&c.posted_jobs[0]).as_deref(),
        Some(GRANT_NAME),
        "the spawned pod carries the grant name as stored in the binding"
    );
}

/// Resolving a grant reads only in-memory bindings. The kubelet materializes
/// the credential from the pod's Secret reference, so the controller has no
/// reason to read the Secret and must not gain one.
///
/// Breaks if resolution adds any apiserver call beyond the Job creation the
/// controller already performs.
#[tokio::test]
async fn resolving_a_grant_reads_no_secret_and_adds_no_api_call() {
    let (svc, cap) = controller(GRANTED_TOOLSET, "Search", vec![]).await;

    svc.begin_tool_call(call("Search", r#"{"__grant":"reader"}"#))
        .await
        .expect("a call naming a member of the bound menu must be accepted");

    let c = cap.lock().unwrap();
    assert!(
        !c.requests.iter().any(|(_, p)| p.contains("/secrets")),
        "the controller must never read the grant's Secret, saw: {:?}",
        c.requests
    );
    let non_job: Vec<_> = c
        .requests
        .iter()
        .filter(|(_, p)| !p.contains("/jobs"))
        .collect();
    assert!(
        non_job.is_empty(),
        "resolution must add no apiserver call beyond Job creation, saw: {non_job:?}"
    );
}

// ---- A non-member value rejects ----

/// Breaks if a grant miss falls through to a default grant or to the grantless
/// path instead of rejecting.
#[tokio::test]
async fn a_call_naming_an_unbound_grant_is_rejected_and_spawns_nothing() {
    let (svc, cap) = controller(GRANTED_TOOLSET, "Search", vec![]).await;

    let result = svc
        .begin_tool_call(call("Search", r#"{"__grant":"writer"}"#))
        .await;

    assert!(
        result.is_err(),
        "a value outside the bound menu is not selectable"
    );
    assert_eq!(
        cap.lock().unwrap().posted_jobs.len(),
        0,
        "a rejected call creates no Job"
    );
}

/// `../reader` has basename `reader`, the one bound grant. Sanitizing the value
/// before comparing — trimming, normalizing, or resolving it as a path — would
/// make it match. It must miss instead.
///
/// Breaks the moment any normalization is applied before the membership test.
#[tokio::test]
async fn a_traversal_shaped_grant_value_misses_rather_than_normalizing_into_a_member() {
    let (svc, cap) = controller(GRANTED_TOOLSET, "Search", vec![]).await;

    let result = svc
        .begin_tool_call(call("Search", r#"{"__grant":"../reader"}"#))
        .await;

    assert!(
        result.is_err(),
        "`__grant` is compared as an exact key, so `../reader` is simply not a member"
    );
    assert_eq!(
        cap.lock().unwrap().posted_jobs.len(),
        0,
        "a non-member value creates no Job"
    );
}

/// A trailing space and a capitalized name are both non-members. Trimming or
/// case-folding would silently admit them.
///
/// Breaks if the comparison is anything other than exact equality.
#[tokio::test]
async fn a_grant_value_differing_only_by_whitespace_or_case_is_not_a_member() {
    for value in ["reader ", " reader", "Reader", "READER"] {
        let (svc, cap) = controller(GRANTED_TOOLSET, "Search", vec![]).await;

        let result = svc
            .begin_tool_call(call("Search", &format!(r#"{{"__grant":"{value}"}}"#)))
            .await;

        assert!(
            result.is_err(),
            "{value:?} is not the bound grant name and must not resolve"
        );
        assert_eq!(
            cap.lock().unwrap().posted_jobs.len(),
            0,
            "{value:?} must create no Job"
        );
    }
}

/// A bare binding offers no menu, so nothing is selectable against it.
///
/// Breaks if a bare entry is given an empty menu and the lookup silently
/// returns a miss-that-could-have-hit, or if the grantless path is taken on a
/// present `__grant`.
#[tokio::test]
async fn a_grant_named_against_a_bare_binding_is_rejected_and_spawns_nothing() {
    let (svc, cap) = controller(BARE_TOOLSET, "Read", vec![]).await;

    let result = svc
        .begin_tool_call(call("Read", r#"{"__grant":"reader"}"#))
        .await;

    assert!(
        result.is_err(),
        "a toolset bound by a bare entry offers no grant to select"
    );
    assert_eq!(
        cap.lock().unwrap().posted_jobs.len(),
        0,
        "a rejected call creates no Job"
    );
}

/// Breaks if a non-string value is coerced to its display form and compared,
/// rather than rejected.
#[tokio::test]
async fn a_non_string_grant_value_is_rejected_and_spawns_nothing() {
    for value in ["123", "true", "null", r#"["reader"]"#, r#"{"n":"reader"}"#] {
        let (svc, cap) = controller(GRANTED_TOOLSET, "Search", vec![]).await;

        let result = svc
            .begin_tool_call(call("Search", &format!(r#"{{"__grant":{value}}}"#)))
            .await;

        assert!(
            result.is_err(),
            "a `__grant` of {value} is not a grant name and must be rejected, not coerced"
        );
        assert_eq!(
            cap.lock().unwrap().posted_jobs.len(),
            0,
            "a `__grant` of {value} must create no Job"
        );
    }
}

// ---- The key is stripped, never forwarded ----

fn query_arg() -> Vec<DiscoveredArgMsg> {
    vec![DiscoveredArgMsg {
        name: "query".into(),
        r#type: "string".into(),
        required: true,
        env: "QUERY".into(),
        description: String::new(),
    }]
}

/// Input validation rejects any key that is not a declared tool arg, so a call
/// carrying `__grant` alongside a valid declared argument is accepted only if
/// the key was removed before validation ran.
///
/// Breaks if the strip moves after validation, or is dropped.
#[tokio::test]
async fn a_call_carrying_a_grant_and_a_declared_argument_is_accepted() {
    let (svc, cap) = controller(GRANTED_TOOLSET, "Search", query_arg()).await;

    svc.begin_tool_call(call(
        "Search",
        r#"{"__grant":"reader","query":"quarterly report"}"#,
    ))
    .await
    .expect("`__grant` must be removed before the declared-argument check runs");

    assert_eq!(
        cap.lock().unwrap().posted_jobs.len(),
        1,
        "the call spawns its tool job once the reserved key is stripped"
    );
}

/// The tool never sees the key or its value. Environment leaks through
/// `/proc/<pid>/environ`, child process inheritance, and logs.
///
/// Breaks if the stripped key is re-added to the tool job's environment under
/// any name, or if the selected grant name is forwarded as a value.
#[tokio::test]
async fn the_reserved_key_reaches_the_tool_job_as_neither_an_env_name_nor_a_value() {
    let (svc, cap) = controller(GRANTED_TOOLSET, "Search", query_arg()).await;

    svc.begin_tool_call(call(
        "Search",
        r#"{"__grant":"reader","query":"quarterly report"}"#,
    ))
    .await
    .expect("`__grant` must be removed before the declared-argument check runs");

    let c = cap.lock().unwrap();
    let env = container_env(&c.posted_jobs[0]);
    assert!(
        !env.iter()
            .any(|e| e.get("name").and_then(|n| n.as_str()) == Some("__grant")),
        "no tool-job env var may be named `__grant`, got: {env:?}"
    );
    assert!(
        !env.iter()
            .any(|e| e.get("value").and_then(|v| v.as_str()) == Some(GRANT_NAME)),
        "the selected grant name must not be forwarded as an env value, got: {env:?}"
    );
}

// ---- Absence is the grantless path ----

/// A binding entry carrying a menu does not oblige a call to select from it.
///
/// Breaks if an absent `__grant` is treated as a miss and rejected, or if a
/// grant is applied by default when the entry carries one.
#[tokio::test]
async fn a_call_naming_no_grant_against_a_grant_bearing_binding_spawns_an_unlabeled_job() {
    let (svc, cap) = controller(GRANTED_TOOLSET, "Search", vec![]).await;

    svc.begin_tool_call(call("Search", "{}"))
        .await
        .expect("an absent `__grant` is the grantless path, not a miss");

    let c = cap.lock().unwrap();
    assert_eq!(c.posted_jobs.len(), 1, "the grantless path still spawns");
    assert_eq!(
        grant_label(&c.posted_jobs[0]),
        None,
        "no selection means no grant label, even where a menu exists"
    );
}
