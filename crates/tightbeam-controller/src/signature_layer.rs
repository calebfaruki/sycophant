//! Tower middleware for the external gRPC listener:
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
//! - **Reject** — anything not in the allowlist (e.g. internal-only
//!   RPCs on a different service, or a streaming-request RPC).
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
/// middleware. Downstream handlers (`gateway.rs::verify_workspace` on the
/// external listener) read this rather than parsing a bearer token.
#[derive(Clone, Debug)]
pub struct VerifiedWorkspace(pub String);

/// Verified client identity (kid) stamped on the request extensions for
/// RPCs that carry no workspace claim. `ListWorkspaces` is the only
/// such RPC today — the handler needs the kid to look up the Enrollment
/// CR's `spec.workspaces`.
#[derive(Clone, Debug)]
pub struct VerifiedClient(pub String);

/// gRPC methods the middleware lets through without signature
/// verification. `RedeemEnrollment` is unauthenticated by design — the
/// signed enrollment code IS the auth artifact.
pub const BYPASS_METHODS: &[&str] = &["/tightbeam.v1.TightbeamGateway/RedeemEnrollment"];

/// gRPC methods the external listener will verify and serve. Anything
/// not in this set OR the bypass set is rejected with
/// `PermissionDenied` — the external listener's surface is deliberately
/// narrow. Additions require an explicit code change so a new RPC
/// can't silently leak to the public internet via tsnet-bridge.
///
/// LLM-dispatch (`Turn`) and the workspace inbound stream (`Subscribe`)
/// have no place here — they live on hangar / the internal listener.
/// The workspace transponder is the sole authority over LLM dispatch for
/// its workspace; external callers reach the agent only through
/// channel-style ingress (`ChannelIngest` → `ChannelReceive`).
pub const ALLOWED_METHODS: &[&str] = &[
    "/tightbeam.v1.TightbeamGateway/MintConversation",
    "/tightbeam.v1.TightbeamGateway/ListConversations",
    // External channel-adapter surface for end-user clients (Flutter
    // app, future SPA). ChannelIngest is the only external path for
    // user input to the agent; ChannelReceive delivers agent replies.
    // The transponder remains the sole LLM-dispatch authority — these
    // RPCs route through the subscriber registry → workspace Subscribe
    // stream → transponder agent loop.
    "/tightbeam.v1.TightbeamGateway/ChannelIngest",
    "/tightbeam.v1.TightbeamGateway/ChannelReceive",
    // History replay for external clients that want to recover missed
    // assistant replies after a disconnect (the conversation log is the
    // durable source of truth; ChannelReceive's push stream is the
    // optimization on top). The handler enforces the workspace-prefix
    // check on conversation_id, so cross-workspace reads are rejected
    // even with the RPC externally reachable.
    "/tightbeam.v1.TightbeamGateway/GetConversationHistory",
    // Turn-phase poll for external clients. Read-only reflection of the
    // controller-owned per-conversation state — it does NOT reach `Turn`
    // and cannot dispatch to the LLM. The handler enforces the same
    // workspace-prefix check on conversation_id as GetConversationHistory,
    // so cross-workspace polls are rejected even with the RPC externally
    // reachable.
    "/tightbeam.v1.TightbeamGateway/GetTurnState",
    // External tool surface. WatchTools streams the per-workspace
    // catalog; CallTool invokes one tool. Tightbeam forwards both to
    // the workspace's transponder, which dispatches via its existing
    // tool_router — NO LLM involvement, so this is tool dispatch, not
    // LLM dispatch.
    "/tightbeam.v1.TightbeamGateway/WatchTools",
    "/tightbeam.v1.TightbeamGateway/CallTool",
    // Conversation lifecycle management. DeleteConversation is
    // immediate and permanent — caller's workspace must own the id;
    // the forward to hangar wipes both the registry and on-disk events.
    "/tightbeam.v1.TightbeamGateway/DeleteConversation",
    // Rename a conversation. Persists to the meta.json sidecar; caller's
    // workspace must own the id. Length cap enforced server-side.
    "/tightbeam.v1.TightbeamGateway/SetConversationName",
    // The external client's abort signal for an in-flight turn; carries
    // a conversation/workspace claim, so caller's workspace must own the id.
    "/tightbeam.v1.TightbeamGateway/CancelTurn",
];

/// gRPC methods the external listener verifies but does NOT bind to a
/// workspace claim. The call's whole purpose is to query the kid's
/// authorization (`ListWorkspaces` returns the Enrollment CR's
/// `spec.workspaces`), so requiring an `x-sig-workspace` header would
/// be circular. The verifier validates signature, kid, body hash,
/// nonce, and timestamp — only the workspace-membership check is
/// dropped. Stays a separate allowlist from `ALLOWED_METHODS` so a
/// mutation that drops the workspace-binding from the standard path
/// cannot silently affect this set.
pub const ALLOWED_NO_WORKSPACE_METHODS: &[&str] =
    &["/tightbeam.v1.TightbeamGateway/ListWorkspaces"];

#[derive(Debug, PartialEq, Eq)]
pub enum MethodClass {
    Bypass,
    VerifyAndForward,
    VerifyNoWorkspace,
    Reject,
}

/// Pure classification of a gRPC method path. Drives the middleware's
/// per-request branch; testable without spinning up a service.
pub fn classify(path: &str) -> MethodClass {
    if BYPASS_METHODS.contains(&path) {
        MethodClass::Bypass
    } else if ALLOWED_METHODS.contains(&path) {
        MethodClass::VerifyAndForward
    } else if ALLOWED_NO_WORKSPACE_METHODS.contains(&path) {
        MethodClass::VerifyNoWorkspace
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
                MethodClass::VerifyNoWorkspace => {
                    let (parts, body) = req.into_parts();
                    let bytes = match body.collect().await {
                        Ok(c) => c.to_bytes(),
                        Err(_) => return Ok(deny("invalid body")),
                    };
                    match verifier
                        .verify_headers_no_workspace(&parts.headers, &path, &bytes)
                        .await
                    {
                        Ok(kid) => {
                            let new_body = Body::new(Full::new(bytes));
                            let mut new_req = Request::from_parts(parts, new_body);
                            new_req.extensions_mut().insert(VerifiedClient(kid));
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
            classify("/tightbeam.v1.TightbeamGateway/RedeemEnrollment"),
            MethodClass::Bypass
        );
    }

    #[test]
    fn classify_rejects_turn() {
        // Turn is the LLM-dispatch RPC on hangar; it has no place on the
        // tightbeam gateway. External clients must not be able to
        // construct system + tools + messages.
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/Turn"),
            MethodClass::Reject
        );
    }

    #[test]
    fn classify_rejects_subscribe() {
        // Subscribe delivers the workspace's inbound user-message stream
        // to the transponder (internal listener). External clients must
        // not eavesdrop on it.
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/Subscribe"),
            MethodClass::Reject
        );
    }

    #[test]
    fn classify_verify_for_mint_conversation() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/MintConversation"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_list_conversations() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/ListConversations"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_channel_ingest() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/ChannelIngest"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_channel_receive() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/ChannelReceive"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_get_turn_state() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/GetTurnState"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_get_conversation_history() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/GetConversationHistory"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_watch_tools_and_call_tool() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/WatchTools"),
            MethodClass::VerifyAndForward
        );
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/CallTool"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_delete_and_set_name() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/DeleteConversation"),
            MethodClass::VerifyAndForward
        );
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/SetConversationName"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_cancel_turn() {
        // CancelTurn is the external client's abort signal for an
        // in-flight turn. The gateway handler and transponder-side guard
        // exist; the ingress allowlist is the sole gap. Without the
        // ALLOWED_METHODS entry, classify falls through to Reject and the
        // client's CancelTurn is denied with PermissionDenied at ingress
        // (the exact manual-e2e failure).
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/CancelTurn"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_rejects_internal_deliver_outbound() {
        // DeliverOutbound lives on TightbeamInternal; it must never be
        // reachable on the external gateway listener.
        assert_eq!(
            classify("/tightbeam.v1.TightbeamInternal/DeliverOutbound"),
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

    // Spy inner service: flips `called` when the middleware forwards a request.
    // The middleware's accept action is `inner.call`, so an un-forwarded request
    // proves the external door refused the presented credential.
    #[derive(Clone)]
    struct SpyService {
        called: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Service<Request<Body>> for SpyService {
        type Response = http::Response<Body>;
        type Error = std::convert::Infallible;
        type Future = std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
        >;
        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(http::Response::new(Body::empty())) })
        }
    }

    // The external listener authenticates ONLY client signatures. A request whose
    // sole credential is a K8s SA / bearer token must be rejected AND never reach
    // the inner service. The inner-not-called assertion is load-bearing — it fails
    // if a future change makes the external door also honor bearer tokens.
    #[tokio::test]
    async fn external_door_rejects_bearer_token_and_does_not_forward() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let called = Arc::new(AtomicBool::new(false));
        let spy = SpyService {
            called: called.clone(),
        };
        let verifier = Arc::new(ClientSignatureVerifier::new(Duration::from_secs(300)));
        let mut mw = SignatureLayer::new(verifier).layer(spy);

        let req = http::Request::builder()
            .method("POST")
            .uri("/tightbeam.v1.TightbeamGateway/MintConversation") // VerifyAndForward
            .header("authorization", "Bearer some-sa-token") // the WRONG credential, alone
            .body(Body::empty())
            .unwrap();

        let resp = mw.call(req).await.unwrap();

        assert_eq!(
            resp.headers().get("grpc-status").unwrap().to_str().unwrap(),
            "7",
            "bearer-only request must be denied (PermissionDenied)"
        );
        assert!(
            !called.load(Ordering::SeqCst),
            "bearer-only request must NOT be forwarded to the inner service"
        );
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
        assert_eq!(BYPASS_METHODS.len(), 1);
        assert_eq!(
            BYPASS_METHODS[0],
            "/tightbeam.v1.TightbeamGateway/RedeemEnrollment"
        );
    }

    #[test]
    fn classify_verify_no_workspace_for_list_workspaces() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamGateway/ListWorkspaces"),
            MethodClass::VerifyNoWorkspace
        );
    }

    #[test]
    fn list_workspaces_is_not_in_workspace_required_allowlist() {
        assert!(
            !ALLOWED_METHODS.contains(&"/tightbeam.v1.TightbeamGateway/ListWorkspaces"),
            "ListWorkspaces must NOT be in the workspace-bound allowlist"
        );
    }

    #[test]
    fn no_workspace_methods_contains_list_workspaces_exactly_once() {
        assert_eq!(ALLOWED_NO_WORKSPACE_METHODS.len(), 1);
        assert_eq!(
            ALLOWED_NO_WORKSPACE_METHODS[0],
            "/tightbeam.v1.TightbeamGateway/ListWorkspaces"
        );
    }

    #[test]
    fn allowed_methods_does_not_include_internal_or_dispatch_rpcs() {
        // Defends against accidentally exposing an internal-only or
        // LLM-dispatch RPC on the external surface.
        let forbidden = [
            "/tightbeam.v1.TightbeamInternal/Subscribe",
            "/tightbeam.v1.TightbeamInternal/SendServerNotification",
            "/tightbeam.v1.TightbeamInternal/SendServerRequestAndAwait",
            "/tightbeam.v1.TightbeamInternal/DeliverOutbound",
            "/hangar.v1.HangarController/Turn",
        ];
        for path in forbidden {
            assert!(
                !ALLOWED_METHODS.contains(&path),
                "{path} must NOT be in the external allowlist",
            );
        }
    }
}
