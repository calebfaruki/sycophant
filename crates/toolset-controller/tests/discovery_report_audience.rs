//! `ReportDiscoveredTools` classifies on the tool-job audience tier.
//!
//! The RPC is presented by the short-lived discovery Job under the
//! `tool.toolset` tool-job audience — the same tier as the six
//! tool-job-dispatch RPCs. The audience classifier is the load-bearing routing
//! seam: a stolen harness token must not reach the report RPC.
//!
//! Materiality: fails if `ReportDiscoveredTools` leaves the tool-job audience
//! set, since it then defaults to the harness tier, letting a stolen harness
//! token drive the controller's tool registry.

use toolset_controller::audience_layer::required_audience_for;

const SVC: &str = "/toolset.v1.ToolsetController";

#[test]
fn report_discovered_tools_requires_the_tool_job_audience_tier() {
    let report = format!("{SVC}/ReportDiscoveredTools");
    // A known tool-job-dispatch RPC and a known harness-dispatch RPC anchor the
    // two tiers without hardcoding the enum variant.
    let tool_job_tier = required_audience_for(&format!("{SVC}/GetToolCall"));
    let harness_tier = required_audience_for(&format!("{SVC}/Turn"));
    assert_ne!(
        tool_job_tier, harness_tier,
        "precondition: the two dispatch surfaces must use distinct audience tiers"
    );

    assert_eq!(
        required_audience_for(&report),
        tool_job_tier,
        "ReportDiscoveredTools must require the tool-job audience tier: the discovery \
         Job presents the tool.toolset token"
    );
    assert_ne!(
        required_audience_for(&report),
        harness_tier,
        "ReportDiscoveredTools must NOT sit on the harness tier: a stolen harness \
         token would otherwise be able to overwrite the controller tool registry"
    );
}
