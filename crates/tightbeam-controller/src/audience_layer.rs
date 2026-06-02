//! Tower middleware for the internal gRPC listener: classifies each
//! incoming method path as either transponder-consumer or llm-dispatch and
//! stamps a `RequiredAudience` extension. The handler's
//! `verify_workspace` reads the extension and selects the matching
//! TokenReview verifier from `InternalVerifierPair`.
//!
//! A transponder-audience token presented against an LLM-dispatch method
//! (or vice versa) fails the audience check at TokenReview time. The
//! audience layer is the routing piece; the verifier pair is the
//! enforcement piece.

use std::task::{Context, Poll};

use http::Request;
use tonic::body::Body;
use tower::{Layer, Service};

/// Required audience for a gRPC method, stamped on request extensions
/// by `RequiredAudienceMiddleware` and read by `pick_verifier`.
///
/// Exhaustive enum so adding a new caller (e.g. a future channel-job
/// audience) forces a compile-time update at every routing decision —
/// there is no fallback branch that silently absorbs an unknown
/// audience.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredAudience {
    Transponder,
    Llm,
}

/// gRPC methods reserved for the LLM-job consumer. Anything not in this
/// list is treated as a transponder-consumer method.
///
/// Adding a method here means that method now requires the
/// `llm.tightbeam.sycophant.md` audience. Be deliberate: a stolen
/// transponder token cannot reach methods in this list.
pub const LLM_METHODS: &[&str] = &[
    "/tightbeam.v1.TightbeamController/GetTurn",
    "/tightbeam.v1.TightbeamController/StreamTurnResult",
];

/// Pure classification of a gRPC method path to its required audience.
/// Unit-testable without spinning up a service.
pub fn required_audience_for(path: &str) -> RequiredAudience {
    if LLM_METHODS.iter().any(|m| *m == path) {
        RequiredAudience::Llm
    } else {
        RequiredAudience::Transponder
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

    #[test]
    fn llm_methods_contains_get_turn_and_stream_turn_result_only() {
        // Defends against either (a) the list being emptied (mutant), or
        // (b) a future PR widening it without an audit. Transponder-bound
        // RPCs must NOT appear here — a stolen transponder token unlocks
        // them otherwise.
        assert_eq!(LLM_METHODS.len(), 2);
        assert!(LLM_METHODS.contains(&"/tightbeam.v1.TightbeamController/GetTurn"));
        assert!(LLM_METHODS.contains(&"/tightbeam.v1.TightbeamController/StreamTurnResult"));
    }

    #[test]
    fn required_audience_for_get_turn_is_llm() {
        assert_eq!(
            required_audience_for("/tightbeam.v1.TightbeamController/GetTurn"),
            RequiredAudience::Llm
        );
    }

    #[test]
    fn required_audience_for_stream_turn_result_is_llm() {
        assert_eq!(
            required_audience_for("/tightbeam.v1.TightbeamController/StreamTurnResult"),
            RequiredAudience::Llm
        );
    }

    #[test]
    fn required_audience_for_turn_is_transponder() {
        // Turn is the high-level transponder-driven LLM call (different
        // from GetTurn, which is the llm-job dequeuing). It must use the
        // transponder audience.
        assert_eq!(
            required_audience_for("/tightbeam.v1.TightbeamController/Turn"),
            RequiredAudience::Transponder
        );
    }

    #[test]
    fn required_audience_for_mint_conversation_is_transponder() {
        assert_eq!(
            required_audience_for("/tightbeam.v1.TightbeamController/MintConversation"),
            RequiredAudience::Transponder
        );
    }

    #[test]
    fn required_audience_for_subscribe_is_transponder() {
        assert_eq!(
            required_audience_for("/tightbeam.v1.TightbeamController/Subscribe"),
            RequiredAudience::Transponder
        );
    }

    #[test]
    fn required_audience_for_channel_methods_is_transponder() {
        for path in &[
            "/tightbeam.v1.TightbeamController/ChannelIngest",
            "/tightbeam.v1.TightbeamController/ChannelReceive",
            "/tightbeam.v1.TightbeamController/ChannelStream",
            "/tightbeam.v1.TightbeamController/ChannelSend",
        ] {
            assert_eq!(required_audience_for(path), RequiredAudience::Transponder);
        }
    }

    #[test]
    fn required_audience_for_unknown_method_defaults_to_transponder() {
        // Fail-closed against fingerprinting: unknown paths take the
        // transponder audience and TokenReview still rejects mismatched
        // tokens. The handler will return Status::Unimplemented anyway.
        assert_eq!(
            required_audience_for("/unknown.Service/Method"),
            RequiredAudience::Transponder
        );
    }
}
