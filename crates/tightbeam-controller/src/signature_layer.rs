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

/// Verified client identity (kid) stamped on the request extensions for
/// RPCs that carry no workspace claim. `ListWorkspaces` is the only
/// such RPC today — the handler needs the kid to look up the Client
/// CR's `spec.workspaces`.
#[derive(Clone, Debug)]
pub struct VerifiedClient(pub String);

/// gRPC methods the middleware lets through without signature
/// verification. `RedeemEnrollment` is unauthenticated by design — the
/// signed enrollment code IS the auth artifact.
pub const BYPASS_METHODS: &[&str] = &["/tightbeam.v1.TightbeamController/RedeemEnrollment"];

/// gRPC methods the external listener will verify and serve. Anything
/// not in this set OR the bypass set is rejected with
/// `PermissionDenied` — the external listener's surface is deliberately
/// narrow. Additions require an explicit code change so a new RPC
/// can't silently leak to the public internet via tsnet-bridge.
///
/// **Turn** and **Subscribe** are deliberately ABSENT from this list.
/// The workspace transponder is the sole authority over LLM dispatch
/// for its workspace (it builds TurnRequests from AGENTS.md + the
/// workspace's tool catalog). External callers reach the agent only
/// through channel-style ingress; they MUST NOT be able to construct
/// the `system + tools + messages` triple that goes to the LLM, and
/// they MUST NOT subscribe to the workspace's inbound user-message
/// stream. Phase 1b will add `ChannelIngest` + `ChannelReceive` here
/// as the replacement path for end-user input + agent reply streaming.
pub const ALLOWED_METHODS: &[&str] = &[
    "/tightbeam.v1.TightbeamController/MintConversation",
    "/tightbeam.v1.TightbeamController/ListConversations",
    // External channel-adapter surface for end-user clients (Flutter
    // app, future SPA). ChannelIngest is the only external path for
    // user input to the agent; ChannelReceive delivers agent replies.
    // The transponder remains the sole LLM-dispatch authority — these
    // RPCs route through state.notify_subscriber → workspace
    // Subscribe stream → transponder agent loop.
    "/tightbeam.v1.TightbeamController/ChannelIngest",
    "/tightbeam.v1.TightbeamController/ChannelReceive",
    // History replay for external clients that want to recover missed
    // assistant replies after a disconnect (the conversation log is the
    // durable source of truth; ChannelReceive's push stream is the
    // optimization on top). The handler enforces the Phase 3.4
    // workspace-prefix check on conversation_id, so cross-workspace
    // reads are rejected even with the RPC externally reachable.
    "/tightbeam.v1.TightbeamController/GetConversationHistory",
    // External tool surface. WatchTools streams the per-workspace
    // catalog; CallTool invokes one tool. Tightbeam forwards both to
    // the workspace's transponder, which dispatches via its existing
    // tool_router — NO LLM involvement, so this is tool dispatch, not
    // LLM dispatch. The system+tools+messages forgery this allowlist
    // guards against still requires reaching `Turn` directly, which
    // remains absent.
    "/tightbeam.v1.TightbeamController/WatchTools",
    "/tightbeam.v1.TightbeamController/CallTool",
    // Conversation lifecycle management. DeleteConversation is
    // immediate and permanent — caller's workspace must own the id;
    // controller wipes both the in-memory registry and on-disk events.
    "/tightbeam.v1.TightbeamController/DeleteConversation",
    // Rename a conversation. Persists to the meta.json sidecar; caller's
    // workspace must own the id. Length cap enforced server-side.
    "/tightbeam.v1.TightbeamController/SetConversationName",
];

/// gRPC methods the external listener verifies but does NOT bind to a
/// workspace claim. The call's whole purpose is to query the kid's
/// authorization (`ListWorkspaces` returns the Client CR's
/// `spec.workspaces`), so requiring an `x-sig-workspace` header would
/// be circular. The verifier validates signature, kid, body hash,
/// nonce, and timestamp — only the workspace-membership check is
/// dropped. Stays a separate allowlist from `ALLOWED_METHODS` so a
/// mutation that drops the workspace-binding from the standard path
/// cannot silently affect this set.
pub const ALLOWED_NO_WORKSPACE_METHODS: &[&str] =
    &["/tightbeam.v1.TightbeamController/ListWorkspaces"];

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
            classify("/tightbeam.v1.TightbeamController/RedeemEnrollment"),
            MethodClass::Bypass
        );
    }

    #[test]
    fn classify_rejects_turn() {
        // Turn is the LLM-dispatch RPC; the transponder is the sole authority
        // over what gets sent to the LLM for a workspace. External clients
        // must not be able to construct system + tools + messages.
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/Turn"),
            MethodClass::Reject
        );
    }

    #[test]
    fn classify_rejects_subscribe() {
        // Subscribe delivers the workspace's inbound user-message stream
        // to the transponder. External clients must not eavesdrop on it.
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/Subscribe"),
            MethodClass::Reject
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
    fn classify_verify_for_channel_ingest() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/ChannelIngest"),
            MethodClass::VerifyAndForward
        );
    }

    #[test]
    fn classify_verify_for_channel_receive() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/ChannelReceive"),
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
    fn classify_verify_no_workspace_for_list_workspaces() {
        assert_eq!(
            classify("/tightbeam.v1.TightbeamController/ListWorkspaces"),
            MethodClass::VerifyNoWorkspace
        );
    }

    #[test]
    fn list_workspaces_is_not_in_workspace_required_allowlist() {
        // Separation of allowlists: ListWorkspaces lives in the
        // no-workspace set, not the standard set. Catches a mutation
        // that promotes it to VerifyAndForward and would then require
        // an x-sig-workspace header that the client never sends.
        assert!(
            !ALLOWED_METHODS.contains(&"/tightbeam.v1.TightbeamController/ListWorkspaces"),
            "ListWorkspaces must NOT be in the workspace-bound allowlist"
        );
    }

    #[test]
    fn no_workspace_methods_contains_list_workspaces_exactly_once() {
        // Defends against a mutation that empties
        // ALLOWED_NO_WORKSPACE_METHODS — would silently reclassify
        // ListWorkspaces as Reject and break device enrollment.
        assert_eq!(ALLOWED_NO_WORKSPACE_METHODS.len(), 1);
        assert_eq!(
            ALLOWED_NO_WORKSPACE_METHODS[0],
            "/tightbeam.v1.TightbeamController/ListWorkspaces"
        );
    }

    #[test]
    fn allowed_methods_does_not_include_streaming_or_internal_rpcs() {
        // Defends against accidentally re-adding any LLM-dispatch /
        // internal-only / streaming RPC to the external surface.
        //
        // - Turn / Subscribe: would let external clients bypass the
        //   transponder (the sole LLM-dispatch authority for the
        //   workspace) or eavesdrop on its inbound stream.
        // - GetTurn / StreamTurnResult: LLM-Job-internal RPCs.
        // - ChannelStream / ChannelSend: streaming, would deadlock the
        //   signature middleware on body-collect.
        let forbidden = [
            "/tightbeam.v1.TightbeamController/Turn",
            "/tightbeam.v1.TightbeamController/Subscribe",
            "/tightbeam.v1.TightbeamController/GetTurn",
            "/tightbeam.v1.TightbeamController/StreamTurnResult",
            "/tightbeam.v1.TightbeamController/ChannelStream",
            "/tightbeam.v1.TightbeamController/ChannelSend",
        ];
        for path in forbidden {
            assert!(
                !ALLOWED_METHODS.contains(&path),
                "{path} must NOT be in the external allowlist",
            );
        }
    }
}
