//! The `ReportDiscoveredTools` RPC handler.
//!
//! The handler feeds the same registry sink the in-process discovery path
//! feeds, so `get_tool` resolves a reported tool afterward. A malformed arg
//! `type` is a terminal request error rather than a retry: the handler rejects
//! it InvalidArgument and registers nothing.
//!
//! Materiality:
//!   test 1 breaks if the handler does not call the registry sink (or keys it
//!   wrongly), so `get_tool` stays None.
//!   test 2 breaks if the handler accepts an unknown arg `type` (or partially
//!   registers before rejecting), so the malformed tool becomes callable.

use std::sync::Arc;

use tonic::{Request, Status};

use toolset_controller::audience_layer::RequiredAudience;
use toolset_controller::grpc::{ControllerService, VerifierPair};
use toolset_controller::registry::ArgType;
use toolset_controller::state::{ControllerState, PromptConfig, WorkspaceBindings};
use toolset_proto::toolset_controller_server::ToolsetController;
use toolset_proto::{DiscoveredArgMsg, DiscoveredToolMsg, ReportDiscoveredToolsRequest};

use shared::auth::TokenVerifier;

/// Verifier that accepts any token as a fixed workspace — the report RPC is
/// keyed by toolset name, not workspace, so identity content is irrelevant.
struct FixedWorkspaceVerifier(String);

#[tonic::async_trait]
impl TokenVerifier for FixedWorkspaceVerifier {
    async fn verify_token(&self, _token: &str) -> Result<String, Status> {
        Ok(self.0.clone())
    }
}

fn state() -> Arc<ControllerState> {
    ControllerState::new(
        None,
        String::new(),
        String::new(),
        shared::scheduling::SchedulingConfig::default(),
    )
}

fn tool_job_service(state: Arc<ControllerState>) -> ControllerService {
    let verifiers = VerifierPair {
        harness: Arc::new(FixedWorkspaceVerifier("ws".into())),
        tool_job: Arc::new(FixedWorkspaceVerifier("ws".into())),
    };
    ControllerService::new(
        state,
        Some(verifiers),
        WorkspaceBindings::empty(),
        PromptConfig::empty(),
    )
}

/// Request stamped as the discovery Job presents it: a tool-job-audience bearer
/// token plus the ToolJob required-audience extension the layer would stamp.
fn tool_job_req<T>(inner: T) -> Request<T> {
    let mut req = Request::new(inner);
    req.metadata_mut()
        .insert("authorization", "Bearer test".parse().unwrap());
    req.extensions_mut().insert(RequiredAudience::ToolJob);
    req
}

#[tokio::test]
async fn report_populates_registry_so_get_tool_resolves() {
    let state = state();
    let svc = tool_job_service(state.clone());

    let req = ReportDiscoveredToolsRequest {
        toolset_name: "stdlib".into(),
        tools: vec![DiscoveredToolMsg {
            name: "Search".into(),
            description: "Search the corpus".into(),
            args: vec![DiscoveredArgMsg {
                name: "query".into(),
                r#type: "string".into(),
                required: true,
                env: "QUERY".into(),
                description: "the query".into(),
            }],
        }],
    };

    svc.report_discovered_tools(tool_job_req(req))
        .await
        .expect("a tool-job-authenticated report must be accepted");

    let tool = state
        .get_tool("Search")
        .await
        .expect("the reported tool must be registered so begin_tool_call resolves it");
    assert_eq!(
        tool.toolset_name, "stdlib",
        "the tool must be keyed to the reported toolset so binding checks pass"
    );
}

#[tokio::test]
async fn report_with_unknown_arg_type_is_rejected_and_registers_nothing() {
    let state = state();
    let svc = tool_job_service(state.clone());

    let req = ReportDiscoveredToolsRequest {
        toolset_name: "stdlib".into(),
        tools: vec![DiscoveredToolMsg {
            name: "Weather".into(),
            description: String::new(),
            args: vec![DiscoveredArgMsg {
                name: "city".into(),
                r#type: "wat".into(), // not string|integer|number|boolean
                required: true,
                env: "CITY".into(),
                description: String::new(),
            }],
        }],
    };

    let status = svc
        .report_discovered_tools(tool_job_req(req))
        .await
        .expect_err("a malformed arg type is terminal and must be rejected");
    assert_eq!(
        status.code(),
        tonic::Code::InvalidArgument,
        "a malformed tool label is a non-retryable (InvalidArgument) request error"
    );

    assert!(
        state.get_tool("Weather").await.is_none(),
        "a rejected report must register nothing — no partial tool set"
    );
}

/// Mutation-killing regression: every non-`string` arg-type string must map to
/// its distinct `ArgType`. Exercises the `integer`/`number`/`boolean` arms of
/// `parse_arg_type` (grpc.rs:851/852/853) that the two prior tests (only
/// `"string"` and `"wat"`) leave untested. Deleting or swapping any of those
/// three arms makes this fail: the arg either round-trips to the wrong `ArgType`
/// or the whole report is rejected as an unknown type.
#[tokio::test]
async fn report_maps_each_non_string_arg_type_to_its_arg_type() {
    let state = state();
    let svc = tool_job_service(state.clone());

    let req = ReportDiscoveredToolsRequest {
        toolset_name: "stdlib".into(),
        tools: vec![DiscoveredToolMsg {
            name: "Typed".into(),
            description: "one arg per non-string scalar type".into(),
            args: vec![
                DiscoveredArgMsg {
                    name: "count".into(),
                    r#type: "integer".into(),
                    required: true,
                    env: "COUNT".into(),
                    description: String::new(),
                },
                DiscoveredArgMsg {
                    name: "ratio".into(),
                    r#type: "number".into(),
                    required: true,
                    env: "RATIO".into(),
                    description: String::new(),
                },
                DiscoveredArgMsg {
                    name: "flag".into(),
                    r#type: "boolean".into(),
                    required: true,
                    env: "FLAG".into(),
                    description: String::new(),
                },
            ],
        }],
    };

    svc.report_discovered_tools(tool_job_req(req))
        .await
        .expect("a report with integer/number/boolean args must be accepted");

    let tool = state
        .get_tool("Typed")
        .await
        .expect("the reported tool must be registered");

    let ty = |name: &str| tool.args.iter().find(|a| a.name == name).unwrap().ty;
    assert_eq!(
        ty("count"),
        ArgType::Integer,
        "an `integer` arg type must round-trip to ArgType::Integer"
    );
    assert_eq!(
        ty("ratio"),
        ArgType::Number,
        "a `number` arg type must round-trip to ArgType::Number"
    );
    assert_eq!(
        ty("flag"),
        ArgType::Boolean,
        "a `boolean` arg type must round-trip to ArgType::Boolean"
    );
}

/// Mutation-killing regression for the arg-description mapping
/// (grpc.rs:819, `(!a.description.is_empty()).then_some(a.description)`).
/// Inverting the `!` drops a non-empty description and fabricates one for an
/// empty report. Asserting both directions on the registered tool kills that
/// mutant: a reported non-empty description must survive as `Some`, and an
/// empty one must register as `None`.
#[tokio::test]
async fn report_maps_arg_description_present_and_absent() {
    let state = state();
    let svc = tool_job_service(state.clone());

    let req = ReportDiscoveredToolsRequest {
        toolset_name: "stdlib".into(),
        tools: vec![DiscoveredToolMsg {
            name: "Described".into(),
            description: "two args, one described one not".into(),
            args: vec![
                DiscoveredArgMsg {
                    name: "query".into(),
                    r#type: "string".into(),
                    required: true,
                    env: "QUERY".into(),
                    description: "the search query".into(),
                },
                DiscoveredArgMsg {
                    name: "limit".into(),
                    r#type: "integer".into(),
                    required: false,
                    env: "LIMIT".into(),
                    description: String::new(),
                },
            ],
        }],
    };

    svc.report_discovered_tools(tool_job_req(req))
        .await
        .expect("a report must be accepted");

    let tool = state
        .get_tool("Described")
        .await
        .expect("the reported tool must be registered");

    let arg = |name: &str| tool.args.iter().find(|a| a.name == name).unwrap();
    assert_eq!(
        arg("query").description.as_deref(),
        Some("the search query"),
        "a reported non-empty arg description must survive as Some"
    );
    assert_eq!(
        arg("limit").description,
        None,
        "an arg reported with an empty description must register as None"
    );
}
