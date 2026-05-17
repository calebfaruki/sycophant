use async_trait::async_trait;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::{SigningKey, VerifyingKey};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use k8s_openapi::api::authentication::v1::{TokenReview, TokenReviewSpec};
use kube::api::PostParams;
use kube::{Api, Client};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::{Request, Status};

#[async_trait]
pub trait TokenVerifier: Send + Sync {
    async fn verify_token(&self, token: &str) -> Result<String, Status>;
}

/// JWT claims for a Tightbeam-issued short-lived bearer token.
///
/// Reserved for the web-session JWT path called out in ADR 013 (no
/// shipping consumer today; the long-lived 90-day device-JWT path was
/// removed in favor of client-generated keypairs). Tokens carry a
/// workspace and an expiry; signed with the controller's per-tenant
/// Ed25519 key. Kept here as the canonical claim shape so the
/// `JwtVerifier` has a concrete type to validate when web sessions
/// ship.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionClaims {
    /// Workspace this token authorizes. Returned by `verify_token` so
    /// downstream gRPC handlers can scope their state lookups.
    pub workspace: String,
    /// Unix-seconds expiry. `jsonwebtoken` enforces `exp` automatically
    /// when the field is present; this struct makes it required (no
    /// `Option`).
    pub exp: i64,
}

/// JWT claims for a one-time enrollment code.
///
/// Operator (or syco-cli wrapper) triggers minting; user presents the
/// code to a client app; app calls `RedeemEnrollment` with a freshly
/// generated public key. Controller validates the code's signature +
/// expiry + claims, then persists the public key on the Client CR.
/// Same Ed25519 signing key as future web-session JWTs — distinguished
/// only by claim shape.
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

/// Sign a one-time enrollment code. Used by the tightbeam-controller's
/// client_watcher when minting a code for a Client CR awaiting
/// enrollment. `ttl_secs` defaults to 3600 (1 hour) at the call site.
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

/// Verifier for Tightbeam-issued short-lived bearer JWTs (`SessionClaims`).
///
/// Reserved for the web-session path called out in ADR 013. Currently
/// has no shipping consumer — the long-lived 90-day device-JWT path was
/// removed in favor of the client-generated-keypair flow handled by
/// `crate::client_signature::ClientSignatureVerifier`. Kept here as
/// the canonical bearer-token verifier so the wiring is ready when web
/// sessions land.
///
/// Validates Ed25519 signatures against the controller's verifying key,
/// enforces expiry, and requires the `workspace`/`exp` claims. Returns
/// the workspace name on success — same trait contract as
/// `K8sTokenVerifier`.
pub struct JwtVerifier {
    decoding_key: DecodingKey,
    validation: Validation,
}

impl JwtVerifier {
    pub fn new(verifying_key: VerifyingKey) -> Self {
        let decoding_key = verifying_key_to_decoding_key(&verifying_key);
        let mut validation = Validation::new(Algorithm::EdDSA);
        // `exp` is the only spec claim we require; `iat`/`nbf`/`aud`/`iss`
        // are not part of the session-token contract.
        validation.required_spec_claims = ["exp".to_string()].into_iter().collect();
        // `serde::Deserialize` on `SessionClaims` enforces `workspace`
        // presence — `jsonwebtoken` will surface a missing-field error
        // from the deserializer, which we map to PermissionDenied.
        Self {
            decoding_key,
            validation,
        }
    }
}

#[async_trait]
impl TokenVerifier for JwtVerifier {
    async fn verify_token(&self, token: &str) -> Result<String, Status> {
        // Every failure mode (bad signature, expired, missing claim,
        // malformed base64, wrong algorithm) collapses to
        // PermissionDenied. The caller has no business distinguishing
        // them — they all mean "this token does not authorize the
        // request." Internal logging at trace level can still expose
        // details for debugging.
        let token_data = decode::<SessionClaims>(token, &self.decoding_key, &self.validation)
            .map_err(|_| Status::permission_denied("invalid token"))?;
        Ok(token_data.claims.workspace)
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
            &SessionClaims {
                workspace: "hello-world".into(),
                exp: now_secs() + 3600,
            },
        );
        let v = JwtVerifier::new(vk);
        let ws = v.verify_token(&jwt).await.unwrap();
        assert_eq!(ws, "hello-world");
    }

    #[tokio::test]
    async fn jwt_verifier_rejects_expired_jwt() {
        // jsonwebtoken's default `exp` validation has a 60s leeway for
        // clock skew. Use a clearly-expired value (1 hour in the past)
        // so this test doesn't false-pass when leeway happens to absorb
        // the offset.
        let (sk, vk) = keypair();
        let jwt = sign(
            &sk,
            &SessionClaims {
                workspace: "hello-world".into(),
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
            &SessionClaims {
                workspace: "hello-world".into(),
                exp: now_secs() + 3600,
            },
        );
        let v = JwtVerifier::new(vk2);
        let err = v.verify_token(&jwt).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn jwt_verifier_rejects_jwt_missing_workspace_claim() {
        // Hand-roll a claims map missing `workspace` — `SessionClaims`
        // would refuse to construct it, so we go through
        // serde_json::Value.
        let (sk, vk) = keypair();
        let claims = serde_json::json!({
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
        // Defends against accidentally returning a hardcoded string
        // instead of the claim value — a mutation test target.
        let (sk, vk) = keypair();
        for ws in ["alpha", "beta-workspace", "x"] {
            let jwt = sign(
                &sk,
                &SessionClaims {
                    workspace: ws.into(),
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
}
