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

/// JWT claims for a one-time enrollment code.
///
/// Operator (or syco-cli wrapper) triggers minting; user presents the
/// code to a client app; app calls `RedeemEnrollment` with a freshly
/// generated public key. Controller validates the code's signature +
/// expiry + claims, then persists the public key on the Client CR.
/// Signed with the per-tenant Ed25519 signing key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentClaims {
    /// Workspace the enrolled client will be scoped to.
    pub workspace: String,
    /// Operator-assigned human-readable client name (e.g. "calebs-iphone").
    pub device_name: String,
    /// UUID for this enrollment code; reserved for one-time-use enforcement.
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

pub struct K8sTokenVerifier {
    client: Client,
    audience: String,
}

impl K8sTokenVerifier {
    pub fn new(client: Client, audience: impl Into<String>) -> Self {
        Self {
            client,
            audience: audience.into(),
        }
    }
}

/// Build the TokenReview that `K8sTokenVerifier::verify_token` would submit.
/// Pure function — separated from the API call so the spec construction
/// (token + audiences) is unit-testable without a kube client.
fn build_token_review(token: &str, audience: &str) -> TokenReview {
    TokenReview {
        metadata: Default::default(),
        spec: TokenReviewSpec {
            token: Some(token.to_string()),
            audiences: Some(vec![audience.to_string()]),
            ..Default::default()
        },
        status: None,
    }
}

#[async_trait]
impl TokenVerifier for K8sTokenVerifier {
    async fn verify_token(&self, token: &str) -> Result<String, Status> {
        let tr = build_token_review(token, &self.audience);
        let token_reviews: Api<TokenReview> = Api::all(self.client.clone());
        let result = token_reviews
            .create(&PostParams::default(), &tr)
            .await
            .map_err(|e| Status::internal(format!("TokenReview API error: {e}")))?;

        workspace_from_review(result, &self.audience)
    }
}

/// Extract the workspace name from a completed `TokenReview`.
///
/// Pure function over the review payload — separated from the API call so the
/// authentication decision logic is unit-testable without a kube client.
#[allow(clippy::result_large_err)]
fn workspace_from_review(review: TokenReview, expected_audience: &str) -> Result<String, Status> {
    let status = review
        .status
        .ok_or_else(|| Status::internal("no TokenReview status"))?;
    if !status.authenticated.unwrap_or(false) {
        return Err(Status::permission_denied("invalid token"));
    }

    // Defense-in-depth: a non-audience-aware authenticator could return
    // `authenticated=true` without checking audience. Require the apiserver to
    // echo back our requested audience in `status.audiences` before trusting
    // the verdict.
    let audience_ok = status
        .audiences
        .as_ref()
        .is_some_and(|auds| auds.iter().any(|a| a == expected_audience));
    if !audience_ok {
        return Err(Status::permission_denied("audience echo mismatch"));
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

/// Path to the SA token mounted into workspace pods (transponder) and
/// in-cluster jobs (tightbeam-llm-job). Workspace pods mount a custom-
/// audience projected token (per `workspace-vap.yaml` — kube-apiserver-
/// audience tokens are forbidden). In-cluster jobs mount a token at the
/// kubelet-default path; they're outside the workspace VAP.
///
/// To keep one literal path across both contexts, the projected volume
/// in `materialize.rs` mounts at `/var/run/secrets/kubernetes.io/serviceaccount`
/// (same as kubelet default) — the audience differs, the path doesn't.
pub const SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

/// Audience for the transponder pod → tightbeam-controller internal
/// listener (Subscribe, Turn, MintConversation, channel methods).
/// Tightbeam pins this audience on TokenReview for transponder-bound
/// methods. Naming convention: `<sender>.<recipient>.sycophant.md` —
/// the sender is the pod kind holding the token (transponder), the
/// recipient is the service consuming it (tightbeam).
pub const TRANSPONDER_TIGHTBEAM_AUDIENCE: &str = "transponder.tightbeam.sycophant.md";

/// Audience for the transponder pod → airlock-controller calls (CallTool,
/// WatchTools). Airlock pins this audience on TokenReview.
pub const TRANSPONDER_AIRLOCK_AUDIENCE: &str = "transponder.airlock.sycophant.md";

/// Audience for the tightbeam-llm-job → tightbeam-controller internal
/// listener (GetTurn, StreamTurnResult). Tightbeam pins this audience on
/// TokenReview for llm-dispatch methods. Leaking a transponder-audience
/// token does not grant llm-dispatch RPCs and vice versa.
pub const LLM_TIGHTBEAM_AUDIENCE: &str = "llm.tightbeam.sycophant.md";

/// Tonic interceptor that injects an SA token as a `Bearer <token>`
/// Authorization header on every outgoing request. The token is
/// re-read from `token_path` on each call so kubelet rotation is
/// observed.
///
/// Parameterized over path so a single process can wield distinct
/// audience-bound tokens against different verifiers: transponder needs
/// one each for tightbeam and airlock; LLM-job uses the kubelet-default
/// path via `default_path()`.
#[derive(Clone, Debug)]
pub struct SaTokenInterceptor {
    token_path: std::path::PathBuf,
}

impl SaTokenInterceptor {
    pub fn new(token_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            token_path: token_path.into(),
        }
    }

    /// The kubelet-default mount path. In-cluster jobs that mount their
    /// projected token at `/var/run/secrets/kubernetes.io/serviceaccount`
    /// (e.g. tightbeam-llm-job) construct via this helper.
    pub fn default_path() -> Self {
        Self::new(SA_TOKEN_PATH)
    }
}

impl tonic::service::Interceptor for SaTokenInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        if let Ok(token) = std::fs::read_to_string(&self.token_path) {
            if let Ok(val) = format!("Bearer {}", token.trim()).parse() {
                request.metadata_mut().insert("authorization", val);
            }
        }
        Ok(request)
    }
}

/// On-disk mount path for the transponder's tightbeam-audience SA token.
/// The materializer mounts the `transponder-auth` projected volume here.
pub const TRANSPONDER_TIGHTBEAM_TOKEN_PATH: &str = "/var/run/secrets/transponder/tightbeam/token";

/// On-disk mount path for the transponder's airlock-audience SA token.
/// The materializer mounts the `transponder-airlock-auth` projected
/// volume here.
pub const TRANSPONDER_AIRLOCK_TOKEN_PATH: &str = "/var/run/secrets/transponder/airlock/token";

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> (SigningKey, VerifyingKey) {
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();
        (signing_key, verifying_key)
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

    const TEST_AUDIENCE: &str = "test.sycophant.md";

    fn review_with(
        authenticated: Option<bool>,
        username: Option<&str>,
        audiences: Option<Vec<&str>>,
    ) -> TokenReview {
        use k8s_openapi::api::authentication::v1::{TokenReviewStatus, UserInfo};
        TokenReview {
            metadata: Default::default(),
            spec: TokenReviewSpec::default(),
            status: Some(TokenReviewStatus {
                authenticated,
                audiences: audiences.map(|a| a.into_iter().map(String::from).collect()),
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
        let review = review_with(
            Some(false),
            Some("system:serviceaccount:ns:sa-alice"),
            Some(vec![TEST_AUDIENCE]),
        );
        let err = workspace_from_review(review, TEST_AUDIENCE).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("invalid token"));
    }

    #[test]
    fn workspace_from_review_missing_authenticated_field_denies() {
        // `authenticated: None` defaults to false via `unwrap_or(false)`.
        let review = review_with(
            None,
            Some("system:serviceaccount:ns:sa-alice"),
            Some(vec![TEST_AUDIENCE]),
        );
        let err = workspace_from_review(review, TEST_AUDIENCE).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn workspace_from_review_authenticated_workspace_sa_returns_name() {
        let review = review_with(
            Some(true),
            Some("system:serviceaccount:ns:sa-hello-world"),
            Some(vec![TEST_AUDIENCE]),
        );
        let ws = workspace_from_review(review, TEST_AUDIENCE).unwrap();
        assert_eq!(ws, "hello-world");
    }

    #[test]
    fn workspace_from_review_authenticated_non_workspace_sa_denies() {
        let review = review_with(
            Some(true),
            Some("system:serviceaccount:ns:default"),
            Some(vec![TEST_AUDIENCE]),
        );
        let err = workspace_from_review(review, TEST_AUDIENCE).unwrap_err();
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
        let err = workspace_from_review(review, TEST_AUDIENCE).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
    }

    #[test]
    fn workspace_from_review_rejects_missing_audiences_echo() {
        let review = review_with(
            Some(true),
            Some("system:serviceaccount:ns:sa-hello-world"),
            None,
        );
        let err = workspace_from_review(review, TEST_AUDIENCE).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("audience echo mismatch"));
    }

    #[test]
    fn workspace_from_review_rejects_empty_audiences_echo() {
        let review = review_with(
            Some(true),
            Some("system:serviceaccount:ns:sa-hello-world"),
            Some(vec![]),
        );
        let err = workspace_from_review(review, TEST_AUDIENCE).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("audience echo mismatch"));
    }

    #[test]
    fn workspace_from_review_rejects_audiences_echo_without_requested() {
        let review = review_with(
            Some(true),
            Some("system:serviceaccount:ns:sa-hello-world"),
            Some(vec!["unrelated.sycophant.md", "other.example.com"]),
        );
        let err = workspace_from_review(review, TEST_AUDIENCE).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("audience echo mismatch"));
    }

    #[test]
    fn workspace_from_review_accepts_audiences_echo_containing_requested() {
        // Multi-audience echo with the requested audience among others — must
        // succeed (catches a future mutant that flips `any` → strict equality).
        let review = review_with(
            Some(true),
            Some("system:serviceaccount:ns:sa-hello-world"),
            Some(vec!["other.example.com", TEST_AUDIENCE]),
        );
        let ws = workspace_from_review(review, TEST_AUDIENCE).unwrap();
        assert_eq!(ws, "hello-world");
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
    fn build_token_review_includes_transponder_tightbeam_audience() {
        let tr = build_token_review("the-token", TRANSPONDER_TIGHTBEAM_AUDIENCE);
        assert_eq!(
            tr.spec.audiences,
            Some(vec![TRANSPONDER_TIGHTBEAM_AUDIENCE.to_string()]),
            "TokenReviewSpec.audiences must carry the configured audience so \
             kube-apiserver rejects tokens minted for other audiences"
        );
    }

    #[test]
    fn build_token_review_includes_transponder_airlock_audience() {
        let tr = build_token_review("the-token", TRANSPONDER_AIRLOCK_AUDIENCE);
        assert_eq!(
            tr.spec.audiences,
            Some(vec![TRANSPONDER_AIRLOCK_AUDIENCE.to_string()]),
        );
    }

    #[test]
    fn build_token_review_includes_llm_tightbeam_audience() {
        let tr = build_token_review("the-token", LLM_TIGHTBEAM_AUDIENCE);
        assert_eq!(
            tr.spec.audiences,
            Some(vec![LLM_TIGHTBEAM_AUDIENCE.to_string()]),
        );
    }

    #[test]
    fn audience_constants_are_distinct() {
        // Leak-prevention invariant: every audience pair must be distinct.
        // If a refactor accidentally aliases two of them, a stolen token of
        // one consumer would unlock the other.
        let all = [
            TRANSPONDER_TIGHTBEAM_AUDIENCE,
            TRANSPONDER_AIRLOCK_AUDIENCE,
            LLM_TIGHTBEAM_AUDIENCE,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "audiences {i} and {j} must differ");
            }
        }
    }

    #[test]
    fn build_token_review_includes_token() {
        let tr = build_token_review("the-token", TRANSPONDER_TIGHTBEAM_AUDIENCE);
        assert_eq!(tr.spec.token, Some("the-token".to_string()));
    }
}
