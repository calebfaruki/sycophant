//! Tower middleware for the internal gRPC listener: classifies each
//! incoming method path as either workspace-consumer or llm-dispatch and
//! stamps a `RequiredAudience` extension. The handler's
//! `verify_workspace` reads the extension and selects the matching
//! TokenReview verifier from `InternalVerifierPair`.
//!
//! A workspace-audience token presented against an LLM-dispatch method
//! (or vice versa) fails the audience check at TokenReview time. The
//! audience layer is the routing piece; the verifier pair is the
//! enforcement piece.

use std::task::{Context, Poll};

use http::Request;
use shared::auth::{LLM_DISPATCH_TIGHTBEAM_AUDIENCE, WORKSPACE_TIGHTBEAM_AUDIENCE};
use tonic::body::Body;
use tower::{Layer, Service};

/// Method-required audience stamped on request extensions. The handler
/// reads this to pick the right `K8sTokenVerifier` from the pair.
#[derive(Clone, Copy, Debug)]
pub struct RequiredAudience(pub &'static str);

/// gRPC methods reserved for the LLM-job consumer. Anything not in this
/// list is treated as a workspace-consumer (transponder) method.
///
/// Adding a method here means that method now requires the
/// `llm-dispatch.tightbeam.sycophant.io` audience. Be deliberate: a
/// stolen workspace token cannot reach methods in this list.
pub const LLM_DISPATCH_METHODS: &[&str] = &[
    "/tightbeam.v1.TightbeamController/GetTurn",
    "/tightbeam.v1.TightbeamController/StreamTurnResult",
];

/// Pure classification of a gRPC method path to its required audience.
/// Unit-testable without spinning up a service.
pub fn required_audience_for(path: &str) -> &'static str {
    if LLM_DISPATCH_METHODS.iter().any(|m| *m == path) {
        LLM_DISPATCH_TIGHTBEAM_AUDIENCE
    } else {
        WORKSPACE_TIGHTBEAM_AUDIENCE
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
        req.extensions_mut().insert(RequiredAudience(audience));
        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_dispatch_methods_contains_get_turn_and_stream_turn_result_only() {
        // Defends against either (a) the list being emptied (mutant), or
        // (b) a future PR widening it without an audit. Workspace-bound
        // RPCs must NOT appear here — a stolen workspace token unlocks
        // them otherwise.
        assert_eq!(LLM_DISPATCH_METHODS.len(), 2);
        assert!(LLM_DISPATCH_METHODS.contains(&"/tightbeam.v1.TightbeamController/GetTurn"));
        assert!(
            LLM_DISPATCH_METHODS.contains(&"/tightbeam.v1.TightbeamController/StreamTurnResult")
        );
    }

    #[test]
    fn required_audience_for_get_turn_is_llm_dispatch() {
        assert_eq!(
            required_audience_for("/tightbeam.v1.TightbeamController/GetTurn"),
            LLM_DISPATCH_TIGHTBEAM_AUDIENCE
        );
    }

    #[test]
    fn required_audience_for_stream_turn_result_is_llm_dispatch() {
        assert_eq!(
            required_audience_for("/tightbeam.v1.TightbeamController/StreamTurnResult"),
            LLM_DISPATCH_TIGHTBEAM_AUDIENCE
        );
    }

    #[test]
    fn required_audience_for_turn_is_workspace() {
        // Turn is the high-level workspace-driven LLM call (different from
        // GetTurn, which is the llm-job dequeuing). It must use the
        // workspace audience.
        assert_eq!(
            required_audience_for("/tightbeam.v1.TightbeamController/Turn"),
            WORKSPACE_TIGHTBEAM_AUDIENCE
        );
    }

    #[test]
    fn required_audience_for_mint_conversation_is_workspace() {
        assert_eq!(
            required_audience_for("/tightbeam.v1.TightbeamController/MintConversation"),
            WORKSPACE_TIGHTBEAM_AUDIENCE
        );
    }

    #[test]
    fn required_audience_for_subscribe_is_workspace() {
        assert_eq!(
            required_audience_for("/tightbeam.v1.TightbeamController/Subscribe"),
            WORKSPACE_TIGHTBEAM_AUDIENCE
        );
    }

    #[test]
    fn required_audience_for_channel_methods_is_workspace() {
        for path in &[
            "/tightbeam.v1.TightbeamController/ChannelIngest",
            "/tightbeam.v1.TightbeamController/ChannelReceive",
            "/tightbeam.v1.TightbeamController/ChannelStream",
            "/tightbeam.v1.TightbeamController/ChannelSend",
        ] {
            assert_eq!(required_audience_for(path), WORKSPACE_TIGHTBEAM_AUDIENCE);
        }
    }

    #[test]
    fn required_audience_for_unknown_method_defaults_to_workspace() {
        // Fail-closed against fingerprinting: unknown paths take the
        // workspace audience and TokenReview still rejects mismatched
        // tokens. The handler will return Status::Unimplemented anyway.
        assert_eq!(
            required_audience_for("/unknown.Service/Method"),
            WORKSPACE_TIGHTBEAM_AUDIENCE
        );
    }

    #[test]
    fn workspace_and_llm_dispatch_audiences_are_distinct() {
        assert_ne!(
            WORKSPACE_TIGHTBEAM_AUDIENCE,
            LLM_DISPATCH_TIGHTBEAM_AUDIENCE
        );
    }
}
