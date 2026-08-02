//! Tower middleware for the internal gRPC listener: classifies each
//! incoming method path as either harness-consumer or llm-dispatch and
//! stamps a `RequiredAudience` extension. The handler's
//! `verify_workspace` reads the extension and selects the matching
//! TokenReview verifier from `InternalVerifierPair`.
//!
//! A harness-audience token presented against an LLM-dispatch method
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
/// Exhaustive enum so adding a new caller forces a compile-time update at
/// every routing decision — there is no fallback branch that silently
/// absorbs an unknown audience.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredAudience {
    Harness,
    Llm,
}

/// gRPC methods reserved for the LLM-job consumer. Anything not in this
/// list is treated as a harness-consumer method.
///
/// Adding a method here means that method now requires the
/// `llm.hangar.sycophant.md` audience. Be deliberate: a stolen
/// harness token cannot reach methods in this list.
pub const LLM_METHODS: &[&str] = &[
    "/hangar.v1.HangarController/GetTurn",
    "/hangar.v1.HangarController/StreamTurnResult",
    "/hangar.v1.HangarController/AwaitTurnCancel",
];

/// Pure classification of a gRPC method path to its required audience.
/// Unit-testable without spinning up a service.
pub fn required_audience_for(path: &str) -> RequiredAudience {
    if LLM_METHODS.contains(&path) {
        RequiredAudience::Llm
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

    #[test]
    fn llm_methods_are_get_turn_stream_turn_result_and_await_turn_cancel() {
        // Defends against either (a) the list being emptied (mutant), or
        // (b) a future PR widening it without an audit. Harness-bound
        // RPCs must NOT appear here — a stolen harness token unlocks
        // them otherwise. AwaitTurnCancel is llm-dispatch: the llm-job
        // long-polls it under its llm-audience token to abandon an
        // in-flight provider call.
        assert_eq!(LLM_METHODS.len(), 3);
        assert!(LLM_METHODS.contains(&"/hangar.v1.HangarController/GetTurn"));
        assert!(LLM_METHODS.contains(&"/hangar.v1.HangarController/StreamTurnResult"));
        assert!(LLM_METHODS.contains(&"/hangar.v1.HangarController/AwaitTurnCancel"));
    }

    #[test]
    fn required_audience_for_get_turn_is_llm() {
        assert_eq!(
            required_audience_for("/hangar.v1.HangarController/GetTurn"),
            RequiredAudience::Llm
        );
    }

    #[test]
    fn required_audience_for_stream_turn_result_is_llm() {
        assert_eq!(
            required_audience_for("/hangar.v1.HangarController/StreamTurnResult"),
            RequiredAudience::Llm
        );
    }

    #[test]
    fn required_audience_for_await_turn_cancel_is_llm() {
        assert_eq!(
            required_audience_for("/hangar.v1.HangarController/AwaitTurnCancel"),
            RequiredAudience::Llm
        );
    }

    #[test]
    fn required_audience_for_turn_is_harness() {
        // Turn is the high-level harness-driven LLM call (different
        // from GetTurn, which is the llm-job dequeuing). It must use the
        // harness audience.
        assert_eq!(
            required_audience_for("/hangar.v1.HangarController/Turn"),
            RequiredAudience::Harness
        );
    }

    #[test]
    fn required_audience_for_get_turn_state_is_harness() {
        // GetTurnState is a client-facing read poll, NOT LLM-dispatch. It
        // must never be promoted into LLM_METHODS — keeping it on the
        // harness audience is the in-cluster half of the guard that
        // `signature_layer` exposes it externally as a non-LLM read.
        assert_eq!(
            required_audience_for("/hangar.v1.HangarController/GetTurnState"),
            RequiredAudience::Harness
        );
    }

    #[test]
    fn required_audience_for_mint_conversation_is_harness() {
        assert_eq!(
            required_audience_for("/hangar.v1.HangarController/MintConversation"),
            RequiredAudience::Harness
        );
    }

    #[test]
    fn required_audience_for_subscribe_is_harness() {
        assert_eq!(
            required_audience_for("/hangar.v1.HangarController/Subscribe"),
            RequiredAudience::Harness
        );
    }

    #[test]
    fn required_audience_for_channel_methods_is_harness() {
        for path in &[
            "/hangar.v1.HangarController/ChannelIngest",
            "/hangar.v1.HangarController/ChannelReceive",
            "/hangar.v1.HangarController/ChannelStream",
            "/hangar.v1.HangarController/ChannelSend",
        ] {
            assert_eq!(required_audience_for(path), RequiredAudience::Harness);
        }
    }

    #[test]
    fn required_audience_for_unknown_method_defaults_to_harness() {
        // Fail-closed against fingerprinting: unknown paths take the
        // harness audience and TokenReview still rejects mismatched
        // tokens. The handler will return Status::Unimplemented anyway.
        assert_eq!(
            required_audience_for("/unknown.Service/Method"),
            RequiredAudience::Harness
        );
    }
}
