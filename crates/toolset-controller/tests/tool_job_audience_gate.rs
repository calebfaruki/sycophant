//! Acceptance test — two-tier TokenReview audience gate on the merged
//! `ToolsetController` service (spec: "Preserved surfaces", AC:
//! "harness token accepted on harness-facing methods / rejected on
//! tool-job-facing methods" and its converse).
//!
//! The tool-job surface requires the `tool.toolset` audience; the harness
//! surface requires `harness.toolset`. The classifier that routes each gRPC
//! method path to its required audience is the load-bearing seam: a stolen
//! harness token must not reach a tool-job RPC, and vice versa.
//!
//! Pinned contract (the coder exposes these):
//!   toolset_controller::audience_layer::TOOL_JOB_METHODS: &[&str]
//!   toolset_controller::audience_layer::required_audience_for(&str) -> T
//!       where T: PartialEq + core::fmt::Debug   (the required-audience tier)
//!
//! Materiality: fails if a tool-job method is dropped from TOOL_JOB_METHODS
//! (leaving it on the harness tier), if a harness method is added to it
//! (letting a stolen harness token reach a tool-job RPC), or if
//! `required_audience_for` stops distinguishing the two tiers.

use toolset_controller::audience_layer::{required_audience_for, TOOL_JOB_METHODS};

const SVC: &str = "/toolset.v1.ToolsetController";

fn tool_job_methods() -> [String; 7] {
    [
        format!("{SVC}/GetTurn"),
        format!("{SVC}/StreamTurnResult"),
        format!("{SVC}/AwaitTurnCancel"),
        format!("{SVC}/GetToolCall"),
        format!("{SVC}/StreamToolResult"),
        format!("{SVC}/AwaitToolCancel"),
        format!("{SVC}/ReportDiscoveredTools"),
    ]
}

fn harness_methods() -> [String; 6] {
    [
        format!("{SVC}/Turn"),
        format!("{SVC}/CancelTurn"),
        format!("{SVC}/WatchTools"),
        format!("{SVC}/BeginToolCall"),
        format!("{SVC}/AwaitToolResult"),
        format!("{SVC}/CancelToolCall"),
    ]
}

#[test]
fn tool_job_methods_are_exactly_the_seven_tool_job_dispatch_rpcs() {
    assert_eq!(
        TOOL_JOB_METHODS.len(),
        7,
        "TOOL_JOB_METHODS must list exactly the seven tool-job-dispatch RPCs"
    );
    for m in tool_job_methods() {
        assert!(
            TOOL_JOB_METHODS.contains(&m.as_str()),
            "tool-job RPC {m} missing from TOOL_JOB_METHODS (would leave it on the harness tier)"
        );
    }
    // Harness-facing RPCs must never be in the tool-job set: a stolen harness
    // token would otherwise unlock a tool-job RPC.
    for m in harness_methods() {
        assert!(
            !TOOL_JOB_METHODS.contains(&m.as_str()),
            "harness RPC {m} must NOT be in TOOL_JOB_METHODS"
        );
    }
}

#[test]
fn tool_job_and_harness_methods_map_to_distinct_audiences() {
    let tool_job_tier = required_audience_for(&tool_job_methods()[0]);
    let harness_tier = required_audience_for(&harness_methods()[0]);
    assert_ne!(
        tool_job_tier, harness_tier,
        "tool-job and harness surfaces must require different audiences"
    );

    for m in tool_job_methods() {
        assert_eq!(
            required_audience_for(&m),
            tool_job_tier,
            "tool-job RPC {m} must require the tool-job audience tier"
        );
    }
    for m in harness_methods() {
        assert_eq!(
            required_audience_for(&m),
            harness_tier,
            "harness RPC {m} must require the harness audience tier"
        );
    }
}
