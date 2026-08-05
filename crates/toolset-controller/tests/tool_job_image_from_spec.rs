//! Acceptance test: the tool-execution Job's container image comes from the
//! AUTHORITATIVE Toolset spec, never from the ephemeral worker's self-report.
//!
//! Security intent under test: a discovery worker reports its tool set over
//! `ReportDiscoveredTools`. That report must NOT be able to choose which image
//! the controller then runs for a tool call. `begin_tool_call` already holds the
//! trusted `Toolset` (it fetches it via `get_toolset` for the credential/egress
//! spec), so the tool-execution Job image must be `toolset.spec.image`.
//!
//! Mutation-killer (the point of this file):
//!   A worker reports a tool; the Toolset spec carries a TRUSTED image. We drive
//!   `begin_tool_call` through a mock kube client that captures the Job it POSTs,
//!   and assert the Job's container image is the TRUSTED spec image. Reverting
//!   the source at grpc.rs:592 from `toolset.spec.image` back to the worker
//!   report (the exact regression this guards) re-reddens it.
//!
//! `ReportDiscoveredToolsRequest` has no `image` field: the worker cannot name
//! an execution image at all, so this also proves requirement 2 (the worker
//! report has zero influence on which image runs) structurally.

use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use kube::client::Body as KubeBody;
use tonic::{Request, Status};

use toolset_controller::audience_layer::RequiredAudience;
use toolset_controller::crd::{Toolset, ToolsetSpec};
use toolset_controller::grpc::{ControllerService, VerifierPair};
use toolset_controller::state::{ControllerState, WorkspaceBindings};
use toolset_proto::toolset_controller_server::ToolsetController;
use toolset_proto::{DiscoveredToolMsg, ReportDiscoveredToolsRequest};

use proto_common::CallToolRequest;
use shared::auth::TokenVerifier;

const TOOLSET: &str = "stdlib";
const WORKSPACE: &str = "ws";
const TRUSTED_IMAGE: &str = "ghcr.io/sycophant/stdlib@sha256:trusted";

/// Verifier that maps any presented token to a single fixed workspace. Both the
/// harness (begin_tool_call) and worker (report) audiences resolve to the same
/// workspace here, which is all the binding check needs.
struct FixedWorkspaceVerifier(String);

#[tonic::async_trait]
impl TokenVerifier for FixedWorkspaceVerifier {
    async fn verify_token(&self, _token: &str) -> Result<String, Status> {
        Ok(self.0.clone())
    }
}

/// A tower service that stands in for the kube API server. It captures the
/// container image of the first Job it is asked to POST, then echoes the request
/// body back as a 201 so `Api::create` deserializes a valid `Job` and returns
/// `Ok`. The captured image is what `begin_tool_call` chose to run.
fn mock_kube_client(captured: Arc<Mutex<Option<String>>>) -> kube::Client {
    let svc = tower::service_fn(move |req: http::Request<KubeBody>| {
        let captured = captured.clone();
        async move {
            let (parts, body) = req.into_parts();
            let bytes = body
                .collect()
                .await
                .expect("mock kube: request body must collect")
                .to_bytes();

            if parts.method == http::Method::POST {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    if let Some(image) = v
                        .pointer("/spec/template/spec/containers/0/image")
                        .and_then(|i| i.as_str())
                    {
                        *captured.lock().unwrap() = Some(image.to_string());
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

fn state_with(client: kube::Client) -> Arc<ControllerState> {
    ControllerState::new(
        Some(client),
        "test-ns".into(),
        "http://toolset-ctrl:9090".into(),
        "ghcr.io/test/prompt-job:latest".into(),
        shared::scheduling::SchedulingConfig::default(),
    )
}

fn service(state: Arc<ControllerState>) -> ControllerService {
    let verifiers = VerifierPair {
        harness: Arc::new(FixedWorkspaceVerifier(WORKSPACE.into())),
        worker: Arc::new(FixedWorkspaceVerifier(WORKSPACE.into())),
    };
    let mut map = std::collections::HashMap::new();
    map.insert(WORKSPACE.to_string(), vec![TOOLSET.to_string()]);
    ControllerService::new(state, Some(verifiers), WorkspaceBindings::from_map(map))
}

fn worker_req<T>(inner: T) -> Request<T> {
    let mut req = Request::new(inner);
    req.metadata_mut()
        .insert("authorization", "Bearer test".parse().unwrap());
    req.extensions_mut().insert(RequiredAudience::Worker);
    req
}

fn harness_req<T>(inner: T) -> Request<T> {
    let mut req = Request::new(inner);
    req.metadata_mut()
        .insert("authorization", "Bearer test".parse().unwrap());
    req.extensions_mut().insert(RequiredAudience::Harness);
    req
}

fn trusted_toolset() -> Toolset {
    Toolset::new(
        TOOLSET,
        ToolsetSpec {
            image: Some(TRUSTED_IMAGE.to_string()),
            credentials: vec![],
            egress: vec![],
            keepalive: false,
        },
    )
}

#[tokio::test]
async fn tool_job_image_comes_from_toolset_spec_not_worker_report() {
    let captured = Arc::new(Mutex::new(None));
    let state = state_with(mock_kube_client(captured.clone()));

    // The authoritative Toolset the controller trusts.
    state.set_toolset(TOOLSET.into(), trusted_toolset()).await;

    let svc = service(state);

    // A worker discovers and reports one tool.
    let report = ReportDiscoveredToolsRequest {
        toolset_name: TOOLSET.into(),
        tools: vec![DiscoveredToolMsg {
            name: "Search".into(),
            description: "Search the corpus".into(),
            args: vec![], // no args -> "{}" is a valid call input
        }],
    };
    svc.report_discovered_tools(worker_req(report))
        .await
        .expect("worker report must be accepted");

    // A harness invokes that tool. This reaches the Job builder + mock POST.
    svc.begin_tool_call(harness_req(CallToolRequest {
        name: "Search".into(),
        input_json: "{}".into(),
        conversation_id: String::new(),
    }))
    .await
    .expect("begin_tool_call must spawn the tool Job");

    let image = captured
        .lock()
        .unwrap()
        .clone()
        .expect("begin_tool_call must have POSTed a tool Job");

    assert_eq!(
        image, TRUSTED_IMAGE,
        "the tool-execution Job image must come from the authoritative \
         toolset.spec.image, not the worker's self-report"
    );
}
