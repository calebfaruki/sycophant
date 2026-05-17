//! Tower middleware for the external gRPC listener (ADR 013 Q4/Q5):
//! buffer each request body, hash it, verify the signed-request
//! envelope described in `shared::client_signature`, then forward
//! with the verified workspace stored in the request's extensions.
//!
//! Three method classes:
//!
//! - **Bypass** — `RedeemEnrollment` flows through unchanged. The
//!   enrollment code IS the auth artifact; no signature is expected.
//! - **VerifyAndForward** — the unary-request RPCs the external
//!   listener accepts. Body bytes get hashed and the SHA-256 is
//!   compared against `x-sig-body-hash` before the request reaches
//!   the inner service.
//! - **Reject** — anything not in the allowlist (e.g. streaming-request
//!   `StreamTurnResult` / `ChannelStream`, or internal-only `GetTurn`).
//!   Returned as `PermissionDenied` so a misrouted internal call isn't
//!   silently accepted.
//!
//! The streaming-incompatibility is real: client-streaming and
//! bidi-streaming bodies arrive in chunks, so a pre-dispatch
//! `body.collect()` would deadlock. We allowlist unary-request methods
//! explicitly rather than detecting streaming at runtime.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::Request;
use http_body_util::{BodyExt, Full};
use shared::client_signature::ClientSignatureVerifier;
use tonic::body::Body;
use tonic::Status;
use tower::{Layer, Service};

/// Verified workspace stamped on the request extensions by the
/// middleware. Downstream handlers (`grpc.rs::verify_workspace` on the
/// external listener) read this rather than parsing a bearer token.
#[derive(Clone, Debug)]
pub struct VerifiedWorkspace(pub String);

/// gRPC methods the middleware lets through without signature
/// verification. `RedeemEnrollment` is unauthenticated by design — the
/// signed enrollment code IS the auth artifact.
pub const BYPASS_METHODS: &[&str] = &["/tightbeam.v1.TightbeamController/RedeemEnrollment"];

/// gRPC methods the external listener will verify and serve. Anything
/// not in this set OR the bypass set is rejected with
/// `PermissionDenied` — the external listener's surface is deliberately
/// narrow. Additions require an explicit code change so a new RPC
/// can't silently leak to the public internet via tsnet-bridge.
pub const ALLOWED_METHODS: &[&str] = &[
    "/tightbeam.v1.TightbeamController/Turn",
    "/tightbeam.v1.TightbeamController/Subscribe",
    "/tightbeam.v1.TightbeamController/MintConversation",
    "/tightbeam.v1.TightbeamController/ListConversations",
];

#[derive(Debug, PartialEq, Eq)]
pub enum MethodClass {
    Bypass,
    VerifyAndForward,
    Reject,
}

/// Pure classification of a gRPC method path. Drives the middleware's
/// per-request branch; testable without spinning up a service.
pub fn classify(path: &str) -> MethodClass {
    if BYPASS_METHODS.iter().any(|m| *m == path) {
        MethodClass::Bypass
    } else if ALLOWED_METHODS.iter().any(|m| *m == path) {
        MethodClass::VerifyAndForward
    } else {
        MethodClass::Reject
    }
}

#[derive(Clone)]
pub struct SignatureLayer {
    verifier: Arc<ClientSignatureVerifier>,
}

impl SignatureLayer {
    pub fn new(verifier: Arc<ClientSignatureVerifier>) -> Self {
        Self { verifier }
    }
}

impl<S> Layer<S> for SignatureLayer {
    type Service = SignatureMiddleware<S>;
    fn layer(&self, inner: S) -> Self::Service {
        SignatureMiddleware {
            inner,
            verifier: self.verifier.clone(),
        }
    }
}

#[derive(Clone)]
pub struct SignatureMiddleware<S> {
    inner: S,
    verifier: Arc<ClientSignatureVerifier>,
}

impl<S> Service<Request<Body>> for SignatureMiddleware<S>
where
    S: Service<Request<Body>, Response = http::Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = http::Response<Body>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        // Pattern from tower docs: replace self.inner with a clone so the
        // moved inner is the one that was poll_ready'd.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let verifier = self.verifier.clone();

        Box::pin(async move {
            let path = req.uri().path().to_string();
            match classify(&path) {
                MethodClass::Bypass => inner.call(req).await,
                MethodClass::Reject => Ok(deny("method not allowed on external listener")),
                MethodClass::VerifyAndForward => {
                    let (parts, body) = req.into_parts();
                    let bytes = match body.collect().await {
                        Ok(c) => c.to_bytes(),
                        Err(_) => return Ok(deny("invalid body")),
                    };
                    match verifier.verify_headers(&parts.headers, &path, &bytes).await {
                        Ok(workspace) => {
                            let new_body = Body::new(Full::new(bytes));
                            let mut new_req = Request::from_parts(parts, new_body);
                            new_req
                                .extensions_mut()
                                .insert(VerifiedWorkspace(workspace));
                            inner.call(new_req).await
                        }
                        Err(_status) => Ok(deny("invalid signature")),
                    }
                }
            }
        })
    }
}

/// Build a `PermissionDenied` gRPC response with a trailer-only status.
/// Tonic clients parse the grpc-status / grpc-message headers into a
/// `Status` on the client side.
fn deny(message: &str) -> http::Response<Body> {
    Status::permission_denied(message).into_http()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_bypass_for_redeem_enrollment() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/RedeemEnrollment"),
            MethodClass::Bypass
        );
    }

    #[test]
    fn classify_verify_for_turn() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/Turn"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_subscribe() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/Subscribe"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_mint_conversation() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/MintConversation"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_list_conversations() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/ListConversations"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_rejects_get_turn() {
        // GetTurn is the LLM Job's internal-only RPC; if it shows up on
        // the external listener that's a routing misconfig.
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/GetTurn"),
            MethodClass::Reject
        );
    }

    #[test]
    fn classify_rejects_stream_turn_result() {
        // Streaming-request RPC; middleware body-collect would deadlock.
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/StreamTurnResult"),
            MethodClass::Reject
        );
    }

    #[test]
    fn classify_rejects_channel_stream() {
        // Bidi-streaming RPC; same reason.
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/ChannelStream"),
            MethodClass::Reject
        );
    }

    #[test]
    fn classify_rejects_channel_send() {
        // Channel-adapter unary RPC — internal-only.
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/ChannelSend"),
            MethodClass::Reject
        );
    }

    #[test]
    fn classify_rejects_unknown_method() {
        assert_eq!(classify("/unknown.Service/Method"), MethodClass::Reject);
    }

    #[test]
    fn classify_rejects_empty_path() {
        assert_eq!(classify(""), MethodClass::Reject);
    }

    #[test]
    fn deny_response_carries_permission_denied_grpc_status() {
        let resp = deny("nope");
        let status = resp
            .headers()
            .get("grpc-status")
            .expect("grpc-status header must be set");
        // tonic::Code::PermissionDenied = 7
        assert_eq!(status.to_str().unwrap(), "7");
    }

    #[test]
    fn deny_response_carries_message_in_grpc_message_header() {
        let resp = deny("nope");
        let msg = resp
            .headers()
            .get("grpc-message")
            .expect("grpc-message header must be set");
        assert_eq!(msg.to_str().unwrap(), "nope");
    }

    #[test]
    fn deny_response_carries_grpc_content_type() {
        let resp = deny("nope");
        let ct = resp
            .headers()
            .get("content-type")
            .expect("content-type must be set");
        assert!(
            ct.to_str().unwrap().starts_with("application/grpc"),
            "content-type must be application/grpc-prefixed, got {:?}",
            ct
        );
    }

    #[test]
    fn bypass_methods_contains_redeem_enrollment_exactly_once() {
        // Defends against a mutation that empties BYPASS_METHODS — would
        // otherwise turn RedeemEnrollment into Reject and break the
        // unauthenticated enrollment flow.
        assert_eq!(BYPASS_METHODS.len(), 1);
        assert_eq!(
            BYPASS_METHODS[0],
            "/tightbeam.v1.TightbeamController/RedeemEnrollment"
        );
    }

    #[test]
    fn allowed_methods_does_not_include_streaming_or_internal_rpcs() {
        // Defends against accidentally adding GetTurn / StreamTurnResult
        // / ChannelStream / ChannelSend to the external-listener
        // surface. Any of those would either be a security leak
        // (ChannelSend) or a deadlock (streaming).
        let forbidden = [
            "/tightbeam.v1.TightbeamController/GetTurn",
            "/tightbeam.v1.TightbeamController/StreamTurnResult",
            "/tightbeam.v1.TightbeamController/ChannelStream",
            "/tightbeam.v1.TightbeamController/ChannelSend",
        ];
        for path in forbidden {
            assert!(
                !ALLOWED_METHODS.iter().any(|m| *m == path),
                "{path} must NOT be in the external allowlist",
            );
        }
    }
}
