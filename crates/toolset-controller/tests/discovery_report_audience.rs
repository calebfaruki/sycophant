//! `ReportDiscoveredTools` classifies on the tool-job audience tier.
//!
//! The RPC is presented by the short-lived discovery Job under the
//! `tool.toolset` tool-job audience. The audience classifier is the
//! load-bearing routing seam: a stolen harness token must not reach the report
//! RPC.
//!
//! Materiality: fails if `ReportDiscoveredTools` leaves the tool-job audience
//! set, since it then defaults to the harness tier, letting a stolen harness
//! token drive the controller's tool registry.

use toolset_controller::audience_layer::{required_audience_for, RequiredAudience};

const SVC: &str = "/toolset.v1.ToolsetController";

#[test]
fn report_discovered_tools_requires_the_tool_job_audience_tier() {
    let report = format!("{SVC}/ReportDiscoveredTools");
    // A real harness-dispatch RPC anchors the harness tier.
    let harness_tier = required_audience_for(&format!("{SVC}/WatchTools"));
    assert_eq!(
        harness_tier,
        RequiredAudience::Harness,
        "precondition: WatchTools is a harness-tier RPC"
    );

    assert_eq!(
        required_audience_for(&report),
        RequiredAudience::ToolJob,
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
