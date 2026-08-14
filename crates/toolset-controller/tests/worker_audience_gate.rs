//! Acceptance test — two-tier TokenReview audience gate on the merged
//! `ToolsetController` service (spec: "Preserved surfaces", AC:
//! "harness token accepted on harness-facing methods / rejected on
//! worker-facing methods" and its converse).
//!
//! The worker surface requires the `tool.toolset` audience; the harness
//! surface requires `harness.toolset`. The classifier that routes each gRPC
//! method path to its required audience is the load-bearing seam: a stolen
//! harness token must not reach a worker RPC, and vice versa.
//!
//! Pinned contract (the coder exposes these):
//!   toolset_controller::audience_layer::WORKER_METHODS: &[&str]
//!   toolset_controller::audience_layer::required_audience_for(&str) -> T
//!       where T: PartialEq + core::fmt::Debug   (the required-audience tier)
//!
//! Materiality: fails if a worker method is dropped from WORKER_METHODS
//! (leaving it on the harness tier), if a harness method is added to it
//! (letting a stolen harness token reach a worker RPC), or if
//! `required_audience_for` stops distinguishing the two tiers.

use toolset_controller::audience_layer::{required_audience_for, WORKER_METHODS};

const SVC: &str = "/toolset.v1.ToolsetController";

fn worker_methods() -> [String; 7] {
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
fn worker_methods_are_exactly_the_seven_worker_dispatch_rpcs() {
    assert_eq!(
        WORKER_METHODS.len(),
        7,
        "WORKER_METHODS must list exactly the seven worker-dispatch RPCs"
    );
    for m in worker_methods() {
        assert!(
            WORKER_METHODS.contains(&m.as_str()),
            "worker RPC {m} missing from WORKER_METHODS (would leave it on the harness tier)"
        );
    }
    // Harness-facing RPCs must never be in the worker set: a stolen harness
    // token would otherwise unlock a worker RPC.
    for m in harness_methods() {
        assert!(
            !WORKER_METHODS.contains(&m.as_str()),
            "harness RPC {m} must NOT be in WORKER_METHODS"
        );
    }
}

#[test]
fn worker_and_harness_methods_map_to_distinct_audiences() {
    let worker_tier = required_audience_for(&worker_methods()[0]);
    let harness_tier = required_audience_for(&harness_methods()[0]);
    assert_ne!(
        worker_tier, harness_tier,
        "worker and harness surfaces must require different audiences"
    );

    for m in worker_methods() {
        assert_eq!(
            required_audience_for(&m),
            worker_tier,
            "worker RPC {m} must require the worker audience tier"
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
