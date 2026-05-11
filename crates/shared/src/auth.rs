use async_trait::async_trait;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::{SigningKey, VerifyingKey};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use k8s_openapi::api::authentication::v1::{TokenReview, TokenReviewSpec};
use kube::api::PostParams;
use kube::{Api, Client};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::{Request, Status};

#[async_trait]
pub trait TokenVerifier: Send + Sync {
    async fn verify_token(&self, token: &str) -> Result<String, Status>;
}

/// JWT claims for a sycophant device token.
///
/// Tokens are signed with the controller's per-deployment Ed25519 key
/// (auto-generated and persisted in the controller's log PVC). The same key
/// signs the short-lived enrollment codes that the operator mints out-of-band
/// — there's exactly one signing identity per controller deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceClaims {
    /// Workspace this device is enrolled to. Returned by `verify_token` so
    /// downstream gRPC handlers can scope their state lookups.
    pub workspace: String,
    /// Server-assigned device identifier (UUID minted at enrollment).
    /// Carried for forensics + future per-device revocation.
    pub device_id: String,
    /// Unix-seconds expiry. `jsonwebtoken` enforces `exp` automatically when
    /// the field is present; this struct makes it required (no `Option`).
    pub exp: i64,
}

/// JWT claims for a one-time enrollment code.
///
/// Operator mints an enrollment code via `tightbeam-controller mint-enrollment
/// <workspace> <device-name>`; user pastes it into the Flutter app; app
/// presents it via `EnrollDevice` RPC; controller validates + exchanges for
/// a long-lived `DeviceClaims` token. Same Ed25519 signing key as device
/// JWTs — distinguished only by claim shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentClaims {
    /// Workspace the enrolled device will be scoped to. Stamped at mint time
    /// (operator picks); copied verbatim into the resulting `DeviceClaims`.
    pub workspace: String,
    /// Operator-assigned human-readable device name (e.g. "calebs-iphone").
    /// Carried into telemetry/forensics; not currently surfaced to the
    /// runtime authz path (`device_id` is what gets stamped into the JWT).
    pub device_name: String,
    /// UUID for this specific enrollment code. Future Phase 3 work will use
    /// this for one-time-use enforcement (denylist of consumed codes).
    pub code_id: String,
    /// Unix-seconds expiry. Short by design (default 1 hour).
    pub exp: i64,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time must be after Unix epoch")
        .as_secs() as i64
}

fn signing_key_to_encoding_key(signing_key: &SigningKey) -> EncodingKey {
    let pkcs8_pem = signing_key
        .to_pkcs8_pem(LineEnding::LF)
        .expect("PKCS#8 PEM serialization is infallible");
    EncodingKey::from_ed_pem(pkcs8_pem.as_bytes())
        .expect("EncodingKey from valid PEM is infallible")
}

fn verifying_key_to_decoding_key(verifying_key: &VerifyingKey) -> DecodingKey {
    let spki_pem = verifying_key
        .to_public_key_pem(LineEnding::LF)
        .expect("VerifyingKey → SPKI PEM is infallible");
    DecodingKey::from_ed_pem(spki_pem.as_bytes()).expect("DecodingKey from valid PEM is infallible")
}

/// Sign a long-lived device JWT. Used by `EnrollDevice` after validating the
/// enrollment code. `ttl_secs` is the lifetime; Phase 2 default is 90 days.
pub fn sign_device_jwt(
    signing_key: &SigningKey,
    workspace: &str,
    device_id: &str,
    ttl_secs: i64,
) -> (String, i64) {
    let exp = now_secs() + ttl_secs;
    let claims = DeviceClaims {
        workspace: workspace.to_string(),
        device_id: device_id.to_string(),
        exp,
    };
    let header = Header::new(Algorithm::EdDSA);
    let encoding_key = signing_key_to_encoding_key(signing_key);
    let jwt = encode(&header, &claims, &encoding_key).expect("device JWT encode is infallible");
    (jwt, exp)
}

/// Sign a one-time enrollment code. Used by the `mint-enrollment` subcommand.
/// `ttl_secs` defaults to 3600 (1 hour) at the call site.
pub fn sign_enrollment_code(
    signing_key: &SigningKey,
    workspace: &str,
    device_name: &str,
    code_id: &str,
    ttl_secs: i64,
) -> String {
    let claims = EnrollmentClaims {
        workspace: workspace.to_string(),
        device_name: device_name.to_string(),
        code_id: code_id.to_string(),
        exp: now_secs() + ttl_secs,
    };
    let header = Header::new(Algorithm::EdDSA);
    let encoding_key = signing_key_to_encoding_key(signing_key);
    encode(&header, &claims, &encoding_key).expect("enrollment code encode is infallible")
}

/// Verify an enrollment code. Returns the decoded claims on success; maps any
/// failure (bad signature, expired, missing claim, malformed) to a single
/// `PermissionDenied` status — the caller has no business distinguishing.
#[allow(clippy::result_large_err)]
pub fn verify_enrollment_code(
    verifying_key: &VerifyingKey,
    code: &str,
) -> Result<EnrollmentClaims, Status> {
    let decoding_key = verifying_key_to_decoding_key(verifying_key);
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.required_spec_claims = ["exp".to_string()].into_iter().collect();
    let token_data = decode::<EnrollmentClaims>(code, &decoding_key, &validation)
        .map_err(|_| Status::permission_denied("invalid enrollment code"))?;
    Ok(token_data.claims)
}

/// Verifier for JWT device tokens issued by `EnrollDevice`.
///
/// Validates Ed25519 signatures against the controller's verifying key,
/// enforces expiry, and requires the `workspace`/`device_id`/`exp` claims.
/// Returns the workspace name on success — same trait contract as the
/// existing `K8sTokenVerifier`, so all gRPC call sites are unchanged.
pub struct JwtVerifier {
    decoding_key: DecodingKey,
    validation: Validation,
}

impl JwtVerifier {
    pub fn new(verifying_key: VerifyingKey) -> Self {
        let decoding_key = verifying_key_to_decoding_key(&verifying_key);
        let mut validation = Validation::new(Algorithm::EdDSA);
        // `exp` is the only spec claim we require; `iat`/`nbf`/`aud`/`iss`
        // are not part of the device-token contract.
        validation.required_spec_claims = ["exp".to_string()].into_iter().collect();
        // `serde::Deserialize` on `DeviceClaims` enforces `workspace` and
        // `device_id` presence — `jsonwebtoken` will surface a missing-field
        // error from the deserializer, which we map to PermissionDenied.
        Self {
            decoding_key,
            validation,
        }
    }
}

#[async_trait]
impl TokenVerifier for JwtVerifier {
    async fn verify_token(&self, token: &str) -> Result<String, Status> {
        // Every failure mode (bad signature, expired, missing claim, malformed
        // base64, wrong algorithm) collapses to PermissionDenied. The caller
        // has no business distinguishing them — they all mean "this token does
        // not authorize the request." Internal logging at trace level can
        // still expose details for debugging.
        let token_data = decode::<DeviceClaims>(token, &self.decoding_key, &self.validation)
            .map_err(|_| Status::permission_denied("invalid token"))?;
        Ok(token_data.claims.workspace)
    }
}

/// Try multiple `TokenVerifier`s in order, return on first success.
///
/// Order is a security property — keep it hardcoded. Cheapest/most-likely
/// verifier first means a failed in-process Ed25519 check (~50µs) precedes
/// a K8s `TokenReview` round-trip (~5–50ms). Sequential trial avoids routing
/// on unverified token claims, which is the documented JWT anti-pattern.
///
/// Errors from individual verifiers are logged at debug for telemetry but
/// flattened to a single `permission_denied("invalid token")` at the wire
/// boundary — clients have no business knowing which verifier rejected them.
///
/// # Load-bearing invariant
///
/// **All accepted token classes must grant equivalent access within a
/// workspace.** Composite verification is safe ONLY because:
/// 1. Every verifier resolves to the same identity shape (a workspace name).
/// 2. All RPCs that consume that identity are workspace-scoped — there are
///    no class-specific endpoints (admin RPCs, internal-only RPCs, etc.).
/// 3. Within a workspace, a device-JWT-authenticated caller and a
///    K8s-SA-token-authenticated caller have identical privilege.
///
/// Workspace is the security perimeter; both routes lead inside it
/// equivalently. The composite cannot enforce class distinctions because
/// it erases them by design.
///
/// # When this invariant breaks (revisit composite design)
///
/// If ANY of these become true, replace the composite with audience-routed
/// per-class verification (each verifier validates an `aud` claim pinning
/// the token to its intended caller class, plus per-RPC interceptor
/// allowlists for class-restricted endpoints):
///
/// - **Class-specific RPCs added.** Any RPC that should accept only one
///   class of caller (e.g., a "rotate signing key" RPC that internal pods
///   must NOT be able to invoke even with a valid workspace SA token).
/// - **Class-specific privileges within a workspace.** If external comms
///   ever need a different scope than internal — read-only-history for
///   mobile vs full read-write for transponder, different rate limits per
///   class, different audit trails — composite erases the distinction.
/// - **Token-confusion attack becomes meaningful.** Today an attacker
///   replaying a device JWT against an internal RPC reaches the same
///   workspace data they already have via the JWT itself. If that stops
///   being true, audience pinning becomes load-bearing.
///
/// Cross-workspace communication is explicitly out of scope and will never
/// be added; do not factor that into design decisions.
pub struct CompositeVerifier {
    verifiers: Vec<Arc<dyn TokenVerifier>>,
}

impl CompositeVerifier {
    pub fn new(verifiers: Vec<Arc<dyn TokenVerifier>>) -> Self {
        assert!(
            !verifiers.is_empty(),
            "CompositeVerifier requires at least one verifier"
        );
        Self { verifiers }
    }
}

#[async_trait]
impl TokenVerifier for CompositeVerifier {
    async fn verify_token(&self, token: &str) -> Result<String, Status> {
        for (idx, v) in self.verifiers.iter().enumerate() {
            match v.verify_token(token).await {
                Ok(workspace) => return Ok(workspace),
                Err(status) => {
                    tracing::debug!(
                        verifier_index = idx,
                        code = ?status.code(),
                        "verifier rejected token"
                    );
                }
            }
        }
        Err(Status::permission_denied("invalid token"))
    }
}

pub struct K8sTokenVerifier {
    client: Client,
}

impl K8sTokenVerifier {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl TokenVerifier for K8sTokenVerifier {
    async fn verify_token(&self, token: &str) -> Result<String, Status> {
        let tr = TokenReview {
            metadata: Default::default(),
            spec: TokenReviewSpec {
                token: Some(token.to_string()),
                ..Default::default()
            },
            status: None,
        };

        let token_reviews: Api<TokenReview> = Api::all(self.client.clone());
        let result = token_reviews
            .create(&PostParams::default(), &tr)
            .await
            .map_err(|e| Status::internal(format!("TokenReview API error: {e}")))?;

        workspace_from_review(result)
    }
}

/// Extract the workspace name from a completed `TokenReview`.
///
/// Pure function over the review payload — separated from the API call so the
/// authentication decision logic is unit-testable without a kube client.
#[allow(clippy::result_large_err)]
pub fn workspace_from_review(review: TokenReview) -> Result<String, Status> {
    let status = review
        .status
        .ok_or_else(|| Status::internal("no TokenReview status"))?;
    if !status.authenticated.unwrap_or(false) {
        return Err(Status::permission_denied("invalid token"));
    }

    let username = status
        .user
        .and_then(|u| u.username)
        .ok_or_else(|| Status::internal("no username in TokenReview"))?;

    let sa_name = username
        .strip_prefix("system:serviceaccount:")
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, sa)| sa)
        .ok_or_else(|| Status::permission_denied("caller is not a ServiceAccount"))?;

    parse_workspace_from_sa(sa_name)
        .map(|ws| ws.to_string())
        .ok_or_else(|| {
            Status::permission_denied(format!("ServiceAccount {sa_name} is not a workspace SA"))
        })
}

#[allow(clippy::result_large_err)]
pub fn extract_bearer_token<T>(request: &Request<T>) -> Result<&str, Status> {
    request
        .metadata()
        .get("authorization")
        .ok_or_else(|| Status::permission_denied("missing authorization metadata"))?
        .to_str()
        .map_err(|_| Status::permission_denied("invalid authorization encoding"))?
        .strip_prefix("Bearer ")
        .ok_or_else(|| Status::permission_denied("authorization must be Bearer token"))
}

pub fn parse_workspace_from_sa(sa_name: &str) -> Option<&str> {
    let workspace = sa_name.strip_prefix("sa-")?;
    if workspace.is_empty() {
        None
    } else {
        Some(workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::Header;

    fn keypair() -> (SigningKey, VerifyingKey) {
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();
        (signing_key, verifying_key)
    }

    fn sign<T: Serialize>(signing_key: &SigningKey, claims: &T) -> String {
        let header = Header::new(Algorithm::EdDSA);
        let encoding_key = signing_key_to_encoding_key(signing_key);
        encode(&header, claims, &encoding_key).expect("sign jwt")
    }

    #[test]
    fn parse_workspace_valid() {
        assert_eq!(
            parse_workspace_from_sa("sa-hello-world"),
            Some("hello-world")
        );
    }

    #[test]
    fn parse_workspace_no_prefix() {
        assert_eq!(parse_workspace_from_sa("default"), None);
    }

    #[test]
    fn parse_workspace_empty_after_prefix() {
        assert_eq!(parse_workspace_from_sa("sa-"), None);
    }

    #[test]
    fn parse_workspace_nested_hyphens() {
        assert_eq!(
            parse_workspace_from_sa("sa-my-workspace"),
            Some("my-workspace")
        );
    }

    #[test]
    fn extract_bearer_valid() {
        let mut req = Request::new(());
        req.metadata_mut()
            .insert("authorization", "Bearer test-token".parse().unwrap());
        assert_eq!(extract_bearer_token(&req).unwrap(), "test-token");
    }

    #[test]
    fn extract_bearer_missing() {
        let req = Request::new(());
        let err = extract_bearer_token(&req).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn extract_bearer_malformed() {
        let mut req = Request::new(());
        req.metadata_mut()
            .insert("authorization", "Basic xxx".parse().unwrap());
        let err = extract_bearer_token(&req).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    fn review_with(authenticated: Option<bool>, username: Option<&str>) -> TokenReview {
        use k8s_openapi::api::authentication::v1::{TokenReviewStatus, UserInfo};
        TokenReview {
            metadata: Default::default(),
            spec: TokenReviewSpec::default(),
            status: Some(TokenReviewStatus {
                authenticated,
                user: username.map(|name| UserInfo {
                    username: Some(name.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn workspace_from_review_unauthenticated_returns_permission_denied() {
        let review = review_with(Some(false), Some("system:serviceaccount:ns:sa-alice"));
        let err = workspace_from_review(review).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("invalid token"));
    }

    #[test]
    fn workspace_from_review_missing_authenticated_field_denies() {
        // `authenticated: None` defaults to false via `unwrap_or(false)`.
        let review = review_with(None, Some("system:serviceaccount:ns:sa-alice"));
        let err = workspace_from_review(review).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn workspace_from_review_authenticated_workspace_sa_returns_name() {
        let review = review_with(Some(true), Some("system:serviceaccount:ns:sa-hello-world"));
        let ws = workspace_from_review(review).unwrap();
        assert_eq!(ws, "hello-world");
    }

    #[test]
    fn workspace_from_review_authenticated_non_workspace_sa_denies() {
        let review = review_with(Some(true), Some("system:serviceaccount:ns:default"));
        let err = workspace_from_review(review).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("not a workspace SA"));
    }

    #[test]
    fn workspace_from_review_no_status_is_internal_error() {
        let review = TokenReview {
            metadata: Default::default(),
            spec: TokenReviewSpec::default(),
            status: None,
        };
        let err = workspace_from_review(review).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
    }

    #[tokio::test]
    async fn jwt_verifier_accepts_valid_jwt_signed_by_matching_key() {
        let (sk, vk) = keypair();
        let jwt = sign(
            &sk,
            &DeviceClaims {
                workspace: "hello-world".into(),
                device_id: "abc-123".into(),
                exp: now_secs() + 3600,
            },
        );
        let v = JwtVerifier::new(vk);
        let ws = v.verify_token(&jwt).await.unwrap();
        assert_eq!(ws, "hello-world");
    }

    #[tokio::test]
    async fn jwt_verifier_rejects_expired_jwt() {
        // jsonwebtoken's default `exp` validation has a 60s leeway for clock
        // skew. Use a clearly-expired value (1 hour in the past) so this test
        // doesn't false-pass when leeway happens to absorb the offset.
        let (sk, vk) = keypair();
        let jwt = sign(
            &sk,
            &DeviceClaims {
                workspace: "hello-world".into(),
                device_id: "abc-123".into(),
                exp: now_secs() - 3600,
            },
        );
        let v = JwtVerifier::new(vk);
        let err = v.verify_token(&jwt).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn jwt_verifier_rejects_jwt_signed_by_different_key() {
        let (sk1, _) = keypair();
        let (_, vk2) = keypair();
        let jwt = sign(
            &sk1,
            &DeviceClaims {
                workspace: "hello-world".into(),
                device_id: "abc-123".into(),
                exp: now_secs() + 3600,
            },
        );
        let v = JwtVerifier::new(vk2);
        let err = v.verify_token(&jwt).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn jwt_verifier_rejects_jwt_missing_workspace_claim() {
        // Hand-roll a claims map missing `workspace` — `DeviceClaims` would
        // refuse to construct it, so we go through serde_json::Value.
        let (sk, vk) = keypair();
        let claims = serde_json::json!({
            "device_id": "abc-123",
            "exp": now_secs() + 3600,
        });
        let jwt = sign(&sk, &claims);
        let v = JwtVerifier::new(vk);
        let err = v.verify_token(&jwt).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn jwt_verifier_rejects_jwt_missing_device_id_claim() {
        let (sk, vk) = keypair();
        let claims = serde_json::json!({
            "workspace": "hello-world",
            "exp": now_secs() + 3600,
        });
        let jwt = sign(&sk, &claims);
        let v = JwtVerifier::new(vk);
        let err = v.verify_token(&jwt).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn jwt_verifier_rejects_jwt_missing_exp_claim() {
        let (sk, vk) = keypair();
        let claims = serde_json::json!({
            "workspace": "hello-world",
            "device_id": "abc-123",
        });
        let jwt = sign(&sk, &claims);
        let v = JwtVerifier::new(vk);
        let err = v.verify_token(&jwt).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn jwt_verifier_rejects_malformed_token() {
        let (_, vk) = keypair();
        let v = JwtVerifier::new(vk);
        let err = v.verify_token("not.a.jwt").await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn jwt_verifier_round_trip_preserves_workspace_name() {
        // Defends against accidentally returning a hardcoded string instead of
        // the claim value — a mutation test target.
        let (sk, vk) = keypair();
        for ws in ["alpha", "beta-workspace", "x"] {
            let jwt = sign(
                &sk,
                &DeviceClaims {
                    workspace: ws.into(),
                    device_id: "abc-123".into(),
                    exp: now_secs() + 3600,
                },
            );
            let v = JwtVerifier::new(vk);
            assert_eq!(v.verify_token(&jwt).await.unwrap(), ws);
        }
    }

    #[test]
    fn sign_enrollment_code_round_trips_through_verify() {
        let (sk, vk) = keypair();
        let now = now_secs();
        let code = sign_enrollment_code(&sk, "hello-world", "calebs-iphone", "code-uuid-1", 3600);
        let claims = verify_enrollment_code(&vk, &code).unwrap();
        assert_eq!(claims.workspace, "hello-world");
        assert_eq!(claims.device_name, "calebs-iphone");
        assert_eq!(claims.code_id, "code-uuid-1");
        // Tight bounds: exp must be roughly now + 3600. Lower bound catches a
        // dropped-ttl mutation; upper bound catches a `+ → *` mutation that
        // would explode exp into the year-millions range.
        assert!(claims.exp > now + 3500, "exp too low: {}", claims.exp);
        assert!(claims.exp < now + 3700, "exp too high: {}", claims.exp);
    }

    #[test]
    fn verify_enrollment_code_rejects_wrong_signing_key() {
        let (sk1, _) = keypair();
        let (_, vk2) = keypair();
        let code = sign_enrollment_code(&sk1, "hello-world", "calebs-iphone", "code-1", 3600);
        let err = verify_enrollment_code(&vk2, &code).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn verify_enrollment_code_rejects_expired_code() {
        // Use a clearly-expired ttl (1 hour in the past) — jsonwebtoken's
        // default leeway absorbs small offsets.
        let (sk, vk) = keypair();
        let code = sign_enrollment_code(&sk, "hello-world", "calebs-iphone", "code-1", -3600);
        let err = verify_enrollment_code(&vk, &code).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn verify_enrollment_code_rejects_malformed_input() {
        let (_, vk) = keypair();
        let err = verify_enrollment_code(&vk, "not.a.jwt").unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn sign_device_jwt_returns_token_acceptable_to_jwt_verifier() {
        let (sk, vk) = keypair();
        let now = now_secs();
        let ttl = 90 * 86_400;
        let (jwt, exp) = sign_device_jwt(&sk, "hello-world", "device-uuid-1", ttl);
        // Tight bounds: exp must be `now + ttl`, not `now * ttl` (which would
        // pass a loose lower-bound check). Window: ±60s around the expected.
        assert!(exp > now + ttl - 60, "exp too low: {}", exp);
        assert!(exp < now + ttl + 60, "exp too high: {}", exp);
        // Spin up a verifier and check the JWT is accepted with the workspace
        // we asked for. This proves the sign/verify pair are wired correctly.
        let v = JwtVerifier::new(vk);
        let rt = v.verify_token(&jwt).await.unwrap();
        assert_eq!(rt, "hello-world");
    }

    #[test]
    fn sign_device_jwt_returns_distinct_exp_at_different_ttls() {
        let (sk, _) = keypair();
        let (_, exp_short) = sign_device_jwt(&sk, "hello-world", "device-1", 60);
        let (_, exp_long) = sign_device_jwt(&sk, "hello-world", "device-1", 60 * 60 * 24);
        assert!(
            exp_long > exp_short,
            "longer TTL must produce later expiry: short={exp_short}, long={exp_long}"
        );
    }

    struct AcceptVerifier(&'static str);
    #[async_trait]
    impl TokenVerifier for AcceptVerifier {
        async fn verify_token(&self, _token: &str) -> Result<String, Status> {
            Ok(self.0.to_string())
        }
    }

    struct RejectVerifier;
    #[async_trait]
    impl TokenVerifier for RejectVerifier {
        async fn verify_token(&self, _token: &str) -> Result<String, Status> {
            Err(Status::permission_denied("nope"))
        }
    }

    #[tokio::test]
    async fn composite_returns_first_success_and_short_circuits() {
        let composite = CompositeVerifier::new(vec![
            Arc::new(AcceptVerifier("first")),
            Arc::new(AcceptVerifier("second")),
        ]);
        assert_eq!(composite.verify_token("any").await.unwrap(), "first");
    }

    #[tokio::test]
    async fn composite_falls_through_to_next_verifier_on_rejection() {
        let composite = CompositeVerifier::new(vec![
            Arc::new(RejectVerifier),
            Arc::new(AcceptVerifier("second")),
        ]);
        assert_eq!(composite.verify_token("any").await.unwrap(), "second");
    }

    #[tokio::test]
    async fn composite_returns_permission_denied_when_all_reject() {
        let composite =
            CompositeVerifier::new(vec![Arc::new(RejectVerifier), Arc::new(RejectVerifier)]);
        let err = composite.verify_token("any").await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        // Generic message — never leak which verifier rejected.
        assert_eq!(err.message(), "invalid token");
    }

    #[test]
    #[should_panic(expected = "at least one verifier")]
    fn composite_panics_on_empty_construction() {
        let _ = CompositeVerifier::new(vec![]);
    }
}
