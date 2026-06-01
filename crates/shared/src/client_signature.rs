//! ECDSA-P256 client-signed request verifier for tightbeam-controller's
//! external listener. Clients (Flutter app, future external channel
//! adapters) hold a P-256 keypair whose public half lives on a Client
//! CR's `status.publicKey`; every external request carries metadata
//! headers describing what was signed and the signature itself.
//!
//! The signed bytes are `method ‖ body_hash ‖ nonce ‖ timestamp`
//! (LF-delimited at the protocol level — see `signed_payload` below).
//! Binding the method and body-hash into the signature prevents an
//! on-device attacker who captures one signed request from pivoting to
//! a different RPC or replaying with a modified body. The nonce +
//! `ReplayCache` close the replay window for an exact-duplicate
//! request.
//!
//! Locked decisions (ADR 013 amendments):
//! - Q4 — only cluster ingress credential. K8s SA tokens are for
//!   in-cluster communications; never accepted on the external listener.
//! - Q5 — full request envelope is signed (not just the nonce).
//! - Q6 — ECDSA P-256 (clients can hardware-bind on iOS Secure Enclave).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tonic::Status;

use crate::replay_cache::ReplayCache;

/// Metadata header carrying the gRPC method path the signature covers.
/// Format: `/tightbeam.v1.TightbeamController/Turn` (the standard
/// gRPC method path). The verifier compares this against the actual
/// dispatched method to reject cross-RPC replays.
pub const SIG_METHOD_HEADER: &str = "x-sig-method";
/// Metadata header carrying the lowercase-hex SHA-256 of the request
/// body bytes. Empty body → 64-char zero string would be unusual;
/// callers send the digest of whatever bytes they encoded.
pub const SIG_BODY_HASH_HEADER: &str = "x-sig-body-hash";
/// Metadata header carrying the client-generated nonce. A nonce is
/// any unique short string per request — clients typically use a
/// UUIDv4. The ReplayCache rejects re-presentations.
pub const SIG_NONCE_HEADER: &str = "x-sig-nonce";
/// Metadata header carrying the client-asserted unix-seconds
/// timestamp. The verifier rejects timestamps outside the cache's
/// freshness window.
pub const SIG_TIMESTAMP_HEADER: &str = "x-sig-timestamp";
/// Metadata header carrying the base64-encoded P-256 ECDSA signature
/// (DER-encoded). The verifier reconstructs `signed_payload(...)` and
/// confirms the signature is valid for the kid's stored VerifyingKey.
pub const SIG_SIGNATURE_HEADER: &str = "x-sig-signature";
/// Metadata header carrying the kid — the Client CR's metadata.name.
/// The verifier uses this to look up the registered public key.
pub const SIG_KID_HEADER: &str = "x-sig-kid";
/// Metadata header carrying the workspace name the client is acting
/// on. The verifier asserts the kid's Client CR authorizes that
/// workspace via `spec.workspaces`.
pub const SIG_WORKSPACE_HEADER: &str = "x-sig-workspace";

/// Compute the bytes the client is required to sign. Public so the
/// Flutter client (and tests on either side) can reconstruct the same
/// payload deterministically. Order matters — the protocol pins it
/// here so cross-language clients produce byte-identical input.
pub fn signed_payload(method: &str, body_hash_hex: &str, nonce: &str, timestamp: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(method.len() + body_hash_hex.len() + nonce.len() + 32);
    out.extend_from_slice(method.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(body_hash_hex.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(nonce.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(timestamp.to_string().as_bytes());
    out
}

/// Compute the lowercase-hex SHA-256 of `bytes`. Callers feed the
/// gRPC-encoded request body in; the result goes into the
/// `x-sig-body-hash` header.
pub fn body_hash_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Cached registration record for a Client CR. The client_watcher
/// populates this from observed Client CRs and removes entries on
/// delete; the verifier reads it on every external request.
#[derive(Clone, Debug)]
pub struct ClientRegistration {
    pub verifying_key: VerifyingKey,
    pub workspaces: Vec<String>,
}

/// Verifier for external client-signed requests. Holds a shared
/// public-key cache (populated by the tightbeam-controller's
/// client_watcher) and a ReplayCache (instance-local to the
/// controller process — bounded memory, lost on restart, which is
/// acceptable because the freshness window is short).
pub struct ClientSignatureVerifier {
    registrations: Arc<RwLock<HashMap<String, ClientRegistration>>>,
    replay: Arc<ReplayCache>,
}

impl ClientSignatureVerifier {
    pub fn new(window: Duration) -> Self {
        Self {
            registrations: Arc::new(RwLock::new(HashMap::new())),
            replay: Arc::new(ReplayCache::new(window)),
        }
    }

    /// Shared handle the client_watcher writes to. Exposed so the
    /// watcher can populate without going through the verifier's own
    /// API surface.
    pub fn registrations(&self) -> Arc<RwLock<HashMap<String, ClientRegistration>>> {
        self.registrations.clone()
    }

    /// Verify a raw HTTP header map (as flowing through a tower
    /// middleware before tonic decodes the body). Returns the
    /// authorized workspace on success. All failure modes collapse to
    /// `PermissionDenied`.
    pub async fn verify_headers(
        &self,
        headers: &http::HeaderMap,
        dispatched_method: &str,
        body_bytes: &[u8],
    ) -> Result<String, Status> {
        let method = read_http_header(headers, SIG_METHOD_HEADER)?;
        let body_hash = read_http_header(headers, SIG_BODY_HASH_HEADER)?;
        let nonce = read_http_header(headers, SIG_NONCE_HEADER)?;
        let timestamp = read_http_header(headers, SIG_TIMESTAMP_HEADER)?
            .parse::<i64>()
            .map_err(|_| permission_denied())?;
        let signature_b64 = read_http_header(headers, SIG_SIGNATURE_HEADER)?;
        let kid = read_http_header(headers, SIG_KID_HEADER)?;
        let workspace = read_http_header(headers, SIG_WORKSPACE_HEADER)?;

        // Cross-RPC defense: the dispatcher tells us which gRPC method
        // is actually about to run. Client claims `x-sig-method`; they
        // must match exactly.
        if method != dispatched_method {
            return Err(permission_denied());
        }

        // Body integrity defense: rehash the bytes and compare.
        let computed_hash = body_hash_hex(body_bytes);
        if !body_hashes_match(&computed_hash, body_hash) {
            return Err(permission_denied());
        }

        // Decode signature. Base64 standard (no URL-safe variant) for
        // wire-format simplicity.
        let signature_bytes = base64_decode(signature_b64).ok_or_else(permission_denied)?;
        let signature = Signature::from_der(&signature_bytes).map_err(|_| permission_denied())?;

        // Look up the client registration. Unknown kid → reject.
        let registration = {
            let map = self.registrations.read().await;
            map.get(kid).cloned().ok_or_else(permission_denied)?
        };

        // Authorization check: the workspace the client claims must be
        // in the Client CR's spec.workspaces list. Defends against a
        // valid client trying to act on a workspace it isn't
        // authorized for.
        if !registration.workspaces.iter().any(|w| w == workspace) {
            return Err(permission_denied());
        }

        // Reconstruct signed payload and verify the signature.
        let payload = signed_payload(method, body_hash, nonce, timestamp);
        registration
            .verifying_key
            .verify(&payload, &signature)
            .map_err(|_| permission_denied())?;

        // Replay-cache check goes last so we don't burn a nonce on a
        // request that would have failed verification anyway.
        if !self.replay.insert_if_fresh(nonce, timestamp) {
            return Err(permission_denied());
        }

        Ok(workspace.to_string())
    }

    /// Verify an external request that carries no workspace claim.
    /// Used by `ListWorkspaces`, where the call IS the authorization
    /// query — there is no workspace to assert. Same envelope as
    /// `verify_headers` minus the workspace header read and the
    /// `spec.workspaces` membership check. Returns the verified kid.
    pub async fn verify_headers_no_workspace(
        &self,
        headers: &http::HeaderMap,
        dispatched_method: &str,
        body_bytes: &[u8],
    ) -> Result<String, Status> {
        let method = read_http_header(headers, SIG_METHOD_HEADER)?;
        let body_hash = read_http_header(headers, SIG_BODY_HASH_HEADER)?;
        let nonce = read_http_header(headers, SIG_NONCE_HEADER)?;
        let timestamp = read_http_header(headers, SIG_TIMESTAMP_HEADER)?
            .parse::<i64>()
            .map_err(|_| permission_denied())?;
        let signature_b64 = read_http_header(headers, SIG_SIGNATURE_HEADER)?;
        let kid = read_http_header(headers, SIG_KID_HEADER)?;

        if method != dispatched_method {
            return Err(permission_denied());
        }

        let computed_hash = body_hash_hex(body_bytes);
        if !body_hashes_match(&computed_hash, body_hash) {
            return Err(permission_denied());
        }

        let signature_bytes = base64_decode(signature_b64).ok_or_else(permission_denied)?;
        let signature = Signature::from_der(&signature_bytes).map_err(|_| permission_denied())?;

        let registration = {
            let map = self.registrations.read().await;
            map.get(kid).cloned().ok_or_else(permission_denied)?
        };

        let payload = signed_payload(method, body_hash, nonce, timestamp);
        registration
            .verifying_key
            .verify(&payload, &signature)
            .map_err(|_| permission_denied())?;

        if !self.replay.insert_if_fresh(nonce, timestamp) {
            return Err(permission_denied());
        }

        Ok(kid.to_string())
    }

    /// Return the workspaces a registered kid is authorized for, or
    /// `None` if the kid is not in the cache. Used by the
    /// `ListWorkspaces` handler after the no-workspace verifier has
    /// established the kid.
    pub async fn get_workspaces_for_kid(&self, kid: &str) -> Option<Vec<String>> {
        self.registrations
            .read()
            .await
            .get(kid)
            .map(|r| r.workspaces.clone())
    }
}

/// Constant-time-adjacent string compare for body hashes. Extracted so
/// the equality check is unit-testable (the inlined `==` previously
/// timed out under mutation testing). Lowercase-hex SHA-256 strings
/// have constant length per hash function, so plain `==` suffices —
/// no timing-side-channel beyond what string equality already exposes.
fn body_hashes_match(computed: &str, claimed: &str) -> bool {
    computed == claimed
}

fn read_http_header<'a>(headers: &'a http::HeaderMap, header: &str) -> Result<&'a str, Status> {
    headers
        .get(header)
        .ok_or_else(permission_denied)?
        .to_str()
        .map_err(|_| permission_denied())
}

fn permission_denied() -> Status {
    // Single uniform message — never leak which header / step failed.
    Status::permission_denied("invalid signature")
}

/// Standard-alphabet base64 decode. Returns None on any malformed
/// input. Free function (no dep on the `base64` crate's runtime API
/// surface beyond decoding) so the verifier stays self-contained.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    STANDARD.decode(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{signature::Signer, SigningKey};
    use p256::elliptic_curve::rand_core::OsRng;

    fn keypair() -> (SigningKey, VerifyingKey) {
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        (sk, vk)
    }

    fn base64_encode(bytes: &[u8]) -> String {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        STANDARD.encode(bytes)
    }

    #[test]
    fn body_hashes_match_returns_true_for_equal_strings() {
        let h = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(body_hashes_match(h, h));
    }

    #[test]
    fn body_hashes_match_returns_false_for_different_strings() {
        let a = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let b = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(!body_hashes_match(a, b));
    }

    #[test]
    fn body_hashes_match_returns_false_for_one_char_off() {
        // Defends `==` vs `<` / `>` / `<=` / `>=` mutations: differ by
        // exactly one char so any non-equality predicate produces a
        // different boolean than `==`.
        let a = "0000000000000000000000000000000000000000000000000000000000000000";
        let b = "0000000000000000000000000000000000000000000000000000000000000001";
        assert!(!body_hashes_match(a, b));
    }

    #[test]
    fn registrations_returns_stable_handle_across_calls() {
        // `registrations()` must return clones of the SAME Arc — the
        // client_watcher writes through this handle and the verifier
        // reads from `self.registrations` directly. Returning fresh
        // Arc<RwLock<HashMap>> on each call would silently divorce
        // writes from reads.
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let h1 = v.registrations();
        let h2 = v.registrations();
        assert!(
            Arc::ptr_eq(&h1, &h2),
            "registrations() must hand out clones of the same Arc"
        );
    }

    /// Build a signed request with valid headers for the given inputs.
    /// Used by tests to vary one field at a time and assert on the
    /// resulting verifier outcome.
    fn signed_headers(
        sk: &SigningKey,
        method: &str,
        body: &[u8],
        nonce: &str,
        ts: i64,
        kid: &str,
        workspace: &str,
    ) -> http::HeaderMap {
        let body_hash = body_hash_hex(body);
        let payload = signed_payload(method, &body_hash, nonce, ts);
        let sig: Signature = sk.sign(&payload);
        let sig_b64 = base64_encode(&sig.to_der().as_bytes());
        let mut h = http::HeaderMap::new();
        h.insert(SIG_METHOD_HEADER, method.parse().unwrap());
        h.insert(SIG_BODY_HASH_HEADER, body_hash.parse().unwrap());
        h.insert(SIG_NONCE_HEADER, nonce.parse().unwrap());
        h.insert(SIG_TIMESTAMP_HEADER, ts.to_string().parse().unwrap());
        h.insert(SIG_SIGNATURE_HEADER, sig_b64.parse().unwrap());
        h.insert(SIG_KID_HEADER, kid.parse().unwrap());
        h.insert(SIG_WORKSPACE_HEADER, workspace.parse().unwrap());
        h
    }

    async fn install(
        verifier: &ClientSignatureVerifier,
        kid: &str,
        vk: VerifyingKey,
        workspaces: Vec<String>,
    ) {
        let map = verifier.registrations();
        map.write().await.insert(
            kid.to_string(),
            ClientRegistration {
                verifying_key: vk,
                workspaces,
            },
        );
    }

    #[tokio::test]
    async fn verify_accepts_well_formed_signature() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let now = current_secs();
        let headers = signed_headers(
            &sk,
            "/tightbeam.v1.TightbeamController/Turn",
            b"some body",
            "nonce-1",
            now,
            "client-alpha",
            "workspace-foo",
        );
        let ws = v
            .verify_headers(
                &headers,
                "/tightbeam.v1.TightbeamController/Turn",
                b"some body",
            )
            .await
            .unwrap();
        assert_eq!(ws, "workspace-foo");
    }

    #[tokio::test]
    async fn verify_rejects_when_dispatched_method_differs_from_signed_method() {
        // Signs for /Turn; dispatcher reports /ListConversations.
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let headers = signed_headers(
            &sk,
            "/tightbeam.v1.TightbeamController/Turn",
            b"body",
            "nonce-1",
            current_secs(),
            "client-alpha",
            "workspace-foo",
        );
        let err = v
            .verify_headers(
                &headers,
                "/tightbeam.v1.TightbeamController/ListConversations",
                b"body",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_rejects_when_body_bytes_were_tampered_after_signing() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let headers = signed_headers(
            &sk,
            "/tightbeam.v1.TightbeamController/Turn",
            b"original body",
            "nonce-1",
            current_secs(),
            "client-alpha",
            "workspace-foo",
        );
        // Dispatcher delivers different bytes than the client signed.
        let err = v
            .verify_headers(
                &headers,
                "/tightbeam.v1.TightbeamController/Turn",
                b"tampered body",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_rejects_stale_timestamp_outside_window() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(60));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let stale = current_secs() - 600;
        let headers = signed_headers(
            &sk,
            "/tightbeam.v1.TightbeamController/Turn",
            b"body",
            "nonce-1",
            stale,
            "client-alpha",
            "workspace-foo",
        );
        let err = v
            .verify_headers(&headers, "/tightbeam.v1.TightbeamController/Turn", b"body")
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_rejects_replayed_nonce_within_window() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let now = current_secs();
        let headers1 = signed_headers(
            &sk,
            "/tightbeam.v1.TightbeamController/Turn",
            b"body",
            "nonce-shared",
            now,
            "client-alpha",
            "workspace-foo",
        );
        let headers2 = signed_headers(
            &sk,
            "/tightbeam.v1.TightbeamController/Turn",
            b"body",
            "nonce-shared",
            now,
            "client-alpha",
            "workspace-foo",
        );
        v.verify_headers(&headers1, "/tightbeam.v1.TightbeamController/Turn", b"body")
            .await
            .unwrap();
        let err = v
            .verify_headers(&headers2, "/tightbeam.v1.TightbeamController/Turn", b"body")
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_rejects_when_kid_is_not_registered() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, _vk) = keypair();
        // Note: never call install — registrations map is empty.
        let headers = signed_headers(
            &sk,
            "/tightbeam.v1.TightbeamController/Turn",
            b"body",
            "nonce-1",
            current_secs(),
            "client-unknown",
            "workspace-foo",
        );
        let err = v
            .verify_headers(&headers, "/tightbeam.v1.TightbeamController/Turn", b"body")
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_rejects_workspace_not_in_clients_authorized_list() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let headers = signed_headers(
            &sk,
            "/tightbeam.v1.TightbeamController/Turn",
            b"body",
            "nonce-1",
            current_secs(),
            "client-alpha",
            "workspace-not-authorized",
        );
        let err = v
            .verify_headers(&headers, "/tightbeam.v1.TightbeamController/Turn", b"body")
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_rejects_when_signature_was_made_by_different_key() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (_sk_real, vk_real) = keypair();
        let (sk_attacker, _) = keypair();
        install(&v, "client-alpha", vk_real, vec!["workspace-foo".into()]).await;
        let headers = signed_headers(
            &sk_attacker,
            "/tightbeam.v1.TightbeamController/Turn",
            b"body",
            "nonce-1",
            current_secs(),
            "client-alpha",
            "workspace-foo",
        );
        let err = v
            .verify_headers(&headers, "/tightbeam.v1.TightbeamController/Turn", b"body")
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_rejects_when_any_required_header_missing() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let headers = signed_headers(
            &sk,
            "/tightbeam.v1.TightbeamController/Turn",
            b"body",
            "nonce-1",
            current_secs(),
            "client-alpha",
            "workspace-foo",
        );
        // Strip one header at a time and confirm each variant is
        // rejected. (Order matches the read-order in verify_headers.)
        for header in &[
            SIG_METHOD_HEADER,
            SIG_BODY_HASH_HEADER,
            SIG_NONCE_HEADER,
            SIG_TIMESTAMP_HEADER,
            SIG_SIGNATURE_HEADER,
            SIG_KID_HEADER,
            SIG_WORKSPACE_HEADER,
        ] {
            let mut subset = headers.clone();
            subset.remove(*header);
            let err = v
                .verify_headers(&subset, "/tightbeam.v1.TightbeamController/Turn", b"body")
                .await
                .unwrap_err();
            assert_eq!(
                err.code(),
                tonic::Code::PermissionDenied,
                "missing header {header} should be PermissionDenied"
            );
        }
    }

    /// Variant of `signed_headers` that omits the x-sig-workspace
    /// header — the shape `ListWorkspaces` clients will use.
    fn signed_headers_no_workspace(
        sk: &SigningKey,
        method: &str,
        body: &[u8],
        nonce: &str,
        ts: i64,
        kid: &str,
    ) -> http::HeaderMap {
        let body_hash = body_hash_hex(body);
        let payload = signed_payload(method, &body_hash, nonce, ts);
        let sig: Signature = sk.sign(&payload);
        let sig_b64 = base64_encode(&sig.to_der().as_bytes());
        let mut h = http::HeaderMap::new();
        h.insert(SIG_METHOD_HEADER, method.parse().unwrap());
        h.insert(SIG_BODY_HASH_HEADER, body_hash.parse().unwrap());
        h.insert(SIG_NONCE_HEADER, nonce.parse().unwrap());
        h.insert(SIG_TIMESTAMP_HEADER, ts.to_string().parse().unwrap());
        h.insert(SIG_SIGNATURE_HEADER, sig_b64.parse().unwrap());
        h.insert(SIG_KID_HEADER, kid.parse().unwrap());
        h
    }

    #[tokio::test]
    async fn verify_no_workspace_accepts_well_formed_signature() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let headers = signed_headers_no_workspace(
            &sk,
            "/tightbeam.v1.TightbeamController/ListWorkspaces",
            b"",
            "nonce-1",
            current_secs(),
            "client-alpha",
        );
        let kid = v
            .verify_headers_no_workspace(
                &headers,
                "/tightbeam.v1.TightbeamController/ListWorkspaces",
                b"",
            )
            .await
            .unwrap();
        assert_eq!(kid, "client-alpha");
    }

    #[tokio::test]
    async fn verify_no_workspace_rejects_when_kid_unknown() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, _vk) = keypair();
        // No install — registrations map is empty.
        let headers = signed_headers_no_workspace(
            &sk,
            "/tightbeam.v1.TightbeamController/ListWorkspaces",
            b"",
            "nonce-1",
            current_secs(),
            "client-unknown",
        );
        let err = v
            .verify_headers_no_workspace(
                &headers,
                "/tightbeam.v1.TightbeamController/ListWorkspaces",
                b"",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_no_workspace_rejects_when_signature_made_by_different_key() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (_sk_real, vk_real) = keypair();
        let (sk_attacker, _) = keypair();
        install(&v, "client-alpha", vk_real, vec!["workspace-foo".into()]).await;
        let headers = signed_headers_no_workspace(
            &sk_attacker,
            "/tightbeam.v1.TightbeamController/ListWorkspaces",
            b"",
            "nonce-1",
            current_secs(),
            "client-alpha",
        );
        let err = v
            .verify_headers_no_workspace(
                &headers,
                "/tightbeam.v1.TightbeamController/ListWorkspaces",
                b"",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_no_workspace_rejects_tampered_body() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let headers = signed_headers_no_workspace(
            &sk,
            "/tightbeam.v1.TightbeamController/ListWorkspaces",
            b"original",
            "nonce-1",
            current_secs(),
            "client-alpha",
        );
        let err = v
            .verify_headers_no_workspace(
                &headers,
                "/tightbeam.v1.TightbeamController/ListWorkspaces",
                b"tampered",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_no_workspace_rejects_replayed_nonce() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let now = current_secs();
        let h1 = signed_headers_no_workspace(
            &sk,
            "/tightbeam.v1.TightbeamController/ListWorkspaces",
            b"",
            "nonce-shared",
            now,
            "client-alpha",
        );
        let h2 = signed_headers_no_workspace(
            &sk,
            "/tightbeam.v1.TightbeamController/ListWorkspaces",
            b"",
            "nonce-shared",
            now,
            "client-alpha",
        );
        v.verify_headers_no_workspace(&h1, "/tightbeam.v1.TightbeamController/ListWorkspaces", b"")
            .await
            .unwrap();
        let err = v
            .verify_headers_no_workspace(
                &h2,
                "/tightbeam.v1.TightbeamController/ListWorkspaces",
                b"",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn verify_no_workspace_rejects_when_any_required_header_missing() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let headers = signed_headers_no_workspace(
            &sk,
            "/tightbeam.v1.TightbeamController/ListWorkspaces",
            b"",
            "nonce-1",
            current_secs(),
            "client-alpha",
        );
        // No-workspace path requires exactly 6 headers (workspace is absent).
        for header in &[
            SIG_METHOD_HEADER,
            SIG_BODY_HASH_HEADER,
            SIG_NONCE_HEADER,
            SIG_TIMESTAMP_HEADER,
            SIG_SIGNATURE_HEADER,
            SIG_KID_HEADER,
        ] {
            let mut subset = headers.clone();
            subset.remove(*header);
            let err = v
                .verify_headers_no_workspace(
                    &subset,
                    "/tightbeam.v1.TightbeamController/ListWorkspaces",
                    b"",
                )
                .await
                .unwrap_err();
            assert_eq!(
                err.code(),
                tonic::Code::PermissionDenied,
                "missing header {header} should be PermissionDenied"
            );
        }
    }

    #[tokio::test]
    async fn verify_no_workspace_ignores_extra_workspace_header() {
        // Documents semantic: an `x-sig-workspace` accidentally sent by
        // a misbehaving client on a ListWorkspaces call is ignored, not
        // rejected. The header is simply unread by this code path.
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (sk, vk) = keypair();
        install(&v, "client-alpha", vk, vec!["workspace-foo".into()]).await;
        let mut headers = signed_headers_no_workspace(
            &sk,
            "/tightbeam.v1.TightbeamController/ListWorkspaces",
            b"",
            "nonce-1",
            current_secs(),
            "client-alpha",
        );
        headers.insert(SIG_WORKSPACE_HEADER, "ignored-anyway".parse().unwrap());
        let kid = v
            .verify_headers_no_workspace(
                &headers,
                "/tightbeam.v1.TightbeamController/ListWorkspaces",
                b"",
            )
            .await
            .unwrap();
        assert_eq!(kid, "client-alpha");
    }

    #[tokio::test]
    async fn get_workspaces_for_kid_returns_installed_workspaces() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        let (_sk, vk) = keypair();
        install(
            &v,
            "client-alpha",
            vk,
            vec!["workspace-a".into(), "workspace-b".into()],
        )
        .await;
        let got = v.get_workspaces_for_kid("client-alpha").await;
        assert_eq!(got, Some(vec!["workspace-a".into(), "workspace-b".into()]));
    }

    #[tokio::test]
    async fn get_workspaces_for_kid_returns_none_for_unknown_kid() {
        let v = ClientSignatureVerifier::new(Duration::from_secs(300));
        assert!(v.get_workspaces_for_kid("client-missing").await.is_none());
    }

    #[test]
    fn signed_payload_is_deterministic_and_ordered() {
        let p1 = signed_payload("/M", "abc", "nonce", 123);
        let p2 = signed_payload("/M", "abc", "nonce", 123);
        assert_eq!(p1, p2);
        // Field-order change must produce different bytes — guards
        // against a refactor that accidentally reshuffles the payload.
        let mixed = signed_payload("/M", "nonce", "abc", 123);
        assert_ne!(p1, mixed);
    }

    #[test]
    fn body_hash_hex_matches_known_sha256() {
        // sha256("") = e3b0...b855 (RFC 6234 test vector).
        assert_eq!(
            body_hash_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    fn current_secs() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }
}
