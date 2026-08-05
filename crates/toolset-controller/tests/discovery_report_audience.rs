//! Acceptance test (AC: "When the controller receives a discovery report over
//! its :9090 gRPC surface from a worker-audience-authenticated discovery Job,
//! the system shall register that toolset's tools").
//!
//! The net-new `ReportDiscoveredTools` RPC is presented by the short-lived
//! discovery Job under the `toolset.toolset` worker audience — the same tier as
//! the six worker-dispatch RPCs. The audience classifier is the load-bearing
//! routing seam: a stolen harness token must not reach the report RPC.
//!
//! Pinned contract (already exposed; the coder only adds the method path to the
//! worker set):
//!   toolset_controller::audience_layer::required_audience_for(&str) -> T
//!       where T: PartialEq + core::fmt::Debug   (the required-audience tier)
//!
//! Materiality: fails if `ReportDiscoveredTools` is NOT added to the worker
//! audience set (it then defaults to the harness tier, letting a stolen harness
//! token drive the controller's tool registry).
//!
//! Red-by-assertion: this file compiles against the current tree and FAILS the
//! assertion today, because `ReportDiscoveredTools` currently classifies as the
//! harness tier (it is not in `WORKER_METHODS`).

use toolset_controller::audience_layer::required_audience_for;

const SVC: &str = "/toolset.v1.ToolsetController";

#[test]
fn report_discovered_tools_requires_the_worker_audience_tier() {
    let report = format!("{SVC}/ReportDiscoveredTools");
    // A known worker-dispatch RPC and a known harness-dispatch RPC anchor the
    // two tiers without hardcoding the enum variant.
    let worker_tier = required_audience_for(&format!("{SVC}/GetToolCall"));
    let harness_tier = required_audience_for(&format!("{SVC}/Turn"));
    assert_ne!(
        worker_tier, harness_tier,
        "precondition: the two dispatch surfaces must use distinct audience tiers"
    );

    assert_eq!(
        required_audience_for(&report),
        worker_tier,
        "ReportDiscoveredTools must require the worker audience tier: the discovery \
         Job presents the toolset.toolset worker token"
    );
    assert_ne!(
        required_audience_for(&report),
        harness_tier,
        "ReportDiscoveredTools must NOT sit on the harness tier: a stolen harness \
         token would otherwise be able to overwrite the controller tool registry"
    );
}
