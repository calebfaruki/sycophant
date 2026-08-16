//! Tower middleware for the single gRPC listener: classifies each incoming
//! method path as either harness-facing or tool-job-facing and stamps a
//! [`RequiredAudience`] extension. The handler's `verify_workspace` reads the
//! extension and selects the matching TokenReview verifier.
//!
//! A harness-audience token presented against a tool-job method (or vice versa)
//! fails the audience check at TokenReview time. The audience layer is the
//! routing piece; the verifier pair is the enforcement piece.

use std::task::{Context, Poll};

use http::Request;
use tonic::body::Body;
use tower::{Layer, Service};

/// Required audience for a gRPC method, stamped on request extensions by
/// [`RequiredAudienceMiddleware`] and read by the handler's verifier pick.
///
/// Exhaustive enum so adding a new caller forces a compile-time update at every
/// routing decision — there is no fallback branch that silently absorbs an
/// unknown audience.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredAudience {
    Harness,
    ToolJob,
}

/// gRPC methods reserved for the spawned tool jobs (the prompt job included).
/// Anything not in this list is treated as a harness-facing method.
///
/// Adding a method here means that method now requires the
/// `tool.toolset.sycophant.md` audience. Be deliberate: a stolen harness
/// token cannot reach methods in this list.
pub const TOOL_JOB_METHODS: &[&str] = &[
    "/toolset.v1.ToolsetController/GetTurn",
    "/toolset.v1.ToolsetController/StreamTurnResult",
    "/toolset.v1.ToolsetController/AwaitTurnCancel",
    "/toolset.v1.ToolsetController/GetToolCall",
    "/toolset.v1.ToolsetController/StreamToolResult",
    "/toolset.v1.ToolsetController/AwaitToolCancel",
    "/toolset.v1.ToolsetController/ReportDiscoveredTools",
];

/// Pure classification of a gRPC method path to its required audience.
/// Unit-testable without spinning up a service.
pub fn required_audience_for(path: &str) -> RequiredAudience {
    if TOOL_JOB_METHODS.contains(&path) {
        RequiredAudience::ToolJob
    } else {
        RequiredAudience::Harness
    }
}

#[derive(Clone)]
pub struct RequiredAudienceLayer;

impl<S> Layer<S> for RequiredAudienceLayer {
    type Service = RequiredAudienceMiddleware<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RequiredAudienceMiddleware { inner }
    }
}

#[derive(Clone)]
pub struct RequiredAudienceMiddleware<S> {
    inner: S,
}

impl<S> Service<Request<Body>> for RequiredAudienceMiddleware<S>
where
    S: Service<Request<Body>, Response = http::Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = http::Response<Body>;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        let audience = required_audience_for(req.uri().path());
        req.extensions_mut().insert(audience);
        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVC: &str = "/toolset.v1.ToolsetController";

    #[test]
    fn tool_job_methods_are_the_seven_tool_job_dispatch_rpcs() {
        // Defends against either (a) the list being emptied (mutant), or (b) a
        // future PR widening it without an audit. Harness-facing RPCs must NOT
        // appear here — a stolen harness token unlocks them otherwise. The
        // three cancel/await long-polls are tool-job-dispatch: the tool job
        // long-polls them under its tool-job-audience token. ReportDiscoveredTools
        // is the discovery Job's tool-job-audience report.
        assert_eq!(TOOL_JOB_METHODS.len(), 7);
        for m in [
            "GetTurn",
            "StreamTurnResult",
            "AwaitTurnCancel",
            "GetToolCall",
            "StreamToolResult",
            "AwaitToolCancel",
            "ReportDiscoveredTools",
        ] {
            assert!(TOOL_JOB_METHODS.contains(&format!("{SVC}/{m}").as_str()));
        }
    }

    #[test]
    fn tool_job_dispatch_methods_require_tool_job_audience() {
        for m in [
            "GetTurn",
            "StreamTurnResult",
            "AwaitTurnCancel",
            "GetToolCall",
            "StreamToolResult",
            "AwaitToolCancel",
        ] {
            assert_eq!(
                required_audience_for(&format!("{SVC}/{m}")),
                RequiredAudience::ToolJob
            );
        }
    }

    #[test]
    fn harness_facing_methods_require_harness_audience() {
        // Turn is the high-level harness-driven dispatch (distinct from
        // GetTurn, the job dequeuing). It and every other harness RPC must
        // use the harness audience.
        for m in [
            "Turn",
            "CancelTurn",
            "WatchTools",
            "BeginToolCall",
            "AwaitToolResult",
            "CancelToolCall",
        ] {
            assert_eq!(
                required_audience_for(&format!("{SVC}/{m}")),
                RequiredAudience::Harness
            );
        }
    }

    #[test]
    fn unknown_method_defaults_to_harness() {
        // Fail-closed against fingerprinting: unknown paths take the harness
        // audience and TokenReview still rejects mismatched tokens. The handler
        // returns Status::Unimplemented anyway.
        assert_eq!(
            required_audience_for("/unknown.Service/Method"),
            RequiredAudience::Harness
        );
    }
}
