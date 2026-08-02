use async_trait::async_trait;
use k8s_openapi::api::authentication::v1::{TokenReview, TokenReviewSpec};
use kube::api::PostParams;
use kube::{Api, Client};
use tonic::{Request, Status};

#[async_trait]
pub trait TokenVerifier: Send + Sync {
    async fn verify_token(&self, token: &str) -> Result<String, Status>;
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

/// Path to the SA token mounted into harness pods and in-cluster
/// jobs (hangar-llm-job). Harness pods mount a custom-audience
/// projected token; the broad pod VAP component-gates the kube-apiserver
/// audience away. In-cluster jobs mount a token at the kubelet-default
/// path; the audience differs, the path doesn't.
pub const SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

/// Audience for the harness pod → hangar-controller internal
/// listener (Subscribe, Turn, MintConversation, channel methods).
/// Hangar pins this audience on TokenReview for harness-bound
/// methods. Naming convention: `<sender>.<recipient>.sycophant.md` —
/// the sender is the pod kind holding the token (harness), the
/// recipient is the service consuming it (hangar).
pub const HARNESS_HANGAR_AUDIENCE: &str = "harness.hangar.sycophant.md";

/// Audience for the harness pod → airlock-controller calls (CallTool,
/// WatchTools). Airlock pins this audience on TokenReview.
pub const HARNESS_AIRLOCK_AUDIENCE: &str = "harness.airlock.sycophant.md";

/// Audience for the harness pod → mainframe-controller calls
/// (WatchTools, CallTool, GetAgent, ListAgents). Mainframe pins this
/// audience on TokenReview.
pub const HARNESS_MAINFRAME_AUDIENCE: &str = "harness.mainframe.sycophant.md";

/// Audience for the hangar-llm-job → hangar-controller internal
/// listener (GetTurn, StreamTurnResult). Hangar pins this audience on
/// TokenReview for llm-dispatch methods. Leaking a harness-audience
/// token does not grant llm-dispatch RPCs and vice versa.
pub const LLM_HANGAR_AUDIENCE: &str = "llm.hangar.sycophant.md";

/// Audience for the chamber (airlock-job) pod → airlock-controller calls (GetToolCall,
/// SendToolResult). The chamber is a distinct sender from the
/// harness, so it carries its own audience rather than reusing
/// HARNESS_AIRLOCK_AUDIENCE. Today airlock-controller does not pin this
/// audience on those methods (they are unauthenticated); the token exists to
/// satisfy the pod VAP's automountServiceAccountToken==false rule and to be
/// the correct sender identity the moment those methods are authenticated.
pub const CHAMBER_AIRLOCK_AUDIENCE: &str = "chamber.airlock.sycophant.md";

/// Audience for the hangar-controller pod → harness pods. The
/// harness exposes a small in-cluster RPC surface (WatchTools,
/// CallTool) that hangar forwards external client calls to. Harness
/// pins this audience on TokenReview to verify the caller is hangar.
pub const HANGAR_HARNESS_AUDIENCE: &str = "hangar.harness.sycophant.md";

/// Audience for the relay-controller pod → hangar-controller. The
/// internet-facing gateway forwards conversation/history RPCs
/// (MintConversation, ListConversations, DeleteConversation,
/// SetConversationName, GetConversationHistory) to hangar, which owns the
/// durable conversation log. Hangar pins this audience on TokenReview to
/// verify the caller is relay.
pub const RELAY_HANGAR_AUDIENCE: &str = "relay.hangar.sycophant.md";

/// Audience for the harness pod → relay-controller internal
/// listener (Subscribe, SendServerNotification, SendServerRequestAndAwait).
/// Relay pins this audience on TokenReview for harness-bound
/// internal methods.
pub const HARNESS_RELAY_AUDIENCE: &str = "harness.relay.sycophant.md";

/// Audience for the hangar-controller pod → relay-controller internal
/// listener (DeliverOutbound). Hangar pushes the assistant reply +
/// terminal turn-state to the gateway in one ordered call. Relay pins
/// this audience on TokenReview to verify the caller is hangar.
pub const HANGAR_RELAY_AUDIENCE: &str = "hangar.relay.sycophant.md";

/// Tonic interceptor that injects an SA token as a `Bearer <token>`
/// Authorization header on every outgoing request. The token is
/// re-read from `token_path` on each call so kubelet rotation is
/// observed.
///
/// Parameterized over path so a single process can wield distinct
/// audience-bound tokens against different verifiers: harness needs
/// one each for hangar and airlock; LLM-job uses the kubelet-default
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
    /// (e.g. hangar-llm-job) construct via this helper.
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

/// On-disk mount path for the harness's hangar-audience SA token.
/// The chart's harness Deployment mounts the `harness-auth`
/// projected volume here.
pub const HARNESS_HANGAR_TOKEN_PATH: &str = "/var/run/secrets/harness/hangar/token";

/// On-disk mount path for the harness's airlock-audience SA token.
/// The chart's harness Deployment mounts the `harness-airlock-auth`
/// projected volume here.
pub const HARNESS_AIRLOCK_TOKEN_PATH: &str = "/var/run/secrets/harness/airlock/token";

/// On-disk mount path for the harness's mainframe-audience SA token.
/// The chart's harness Deployment mounts the `harness-mainframe-auth`
/// projected volume here.
pub const HARNESS_MAINFRAME_TOKEN_PATH: &str = "/var/run/secrets/harness/mainframe/token";

/// On-disk mount path for the harness's relay-audience SA token.
/// The chart's harness Deployment mounts the `harness-relay-auth`
/// projected volume here. Used by the harness to dial the gateway's
/// internal listener (Subscribe + the channel server-request methods).
pub const HARNESS_RELAY_TOKEN_PATH: &str = "/var/run/secrets/harness/relay/token";

/// On-disk mount path for the hangar-controller's harness-audience
/// SA token. The chart's hangar-ctrl Deployment mounts a projected
/// volume here. Used by hangar to dial per-workspace harness pods
/// when forwarding external `CallTool`/`WatchTools` calls.
pub const HANGAR_HARNESS_TOKEN_PATH: &str = "/var/run/secrets/hangar/harness/token";

/// On-disk mount path for the relay-controller's hangar-audience SA
/// token. The chart's relay-ctrl Deployment mounts a projected
/// volume here. Used by relay to dial hangar when forwarding external
/// conversation/history RPCs.
pub const RELAY_HANGAR_TOKEN_PATH: &str = "/var/run/secrets/relay/hangar/token";

/// On-disk mount path for the hangar-controller's relay-audience SA
/// token. The chart's hangar-ctrl Deployment mounts a projected volume
/// here. Used by hangar to dial the gateway's `DeliverOutbound` when
/// pushing the assistant reply + terminal turn-state to the client.
pub const HANGAR_RELAY_TOKEN_PATH: &str = "/var/run/secrets/hangar/relay/token";

#[cfg(test)]
mod tests {
    use super::*;

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
    fn build_token_review_includes_harness_hangar_audience() {
        let tr = build_token_review("the-token", HARNESS_HANGAR_AUDIENCE);
        assert_eq!(
            tr.spec.audiences,
            Some(vec![HARNESS_HANGAR_AUDIENCE.to_string()]),
            "TokenReviewSpec.audiences must carry the configured audience so \
             kube-apiserver rejects tokens minted for other audiences"
        );
    }

    #[test]
    fn build_token_review_includes_harness_airlock_audience() {
        let tr = build_token_review("the-token", HARNESS_AIRLOCK_AUDIENCE);
        assert_eq!(
            tr.spec.audiences,
            Some(vec![HARNESS_AIRLOCK_AUDIENCE.to_string()]),
        );
    }

    #[test]
    fn build_token_review_includes_llm_hangar_audience() {
        let tr = build_token_review("the-token", LLM_HANGAR_AUDIENCE);
        assert_eq!(
            tr.spec.audiences,
            Some(vec![LLM_HANGAR_AUDIENCE.to_string()]),
        );
    }

    #[test]
    fn build_token_review_includes_chamber_airlock_audience() {
        let tr = build_token_review("the-token", CHAMBER_AIRLOCK_AUDIENCE);
        assert_eq!(
            tr.spec.audiences,
            Some(vec![CHAMBER_AIRLOCK_AUDIENCE.to_string()]),
        );
    }

    #[test]
    fn build_token_review_includes_harness_relay_audience() {
        let tr = build_token_review("the-token", HARNESS_RELAY_AUDIENCE);
        assert_eq!(
            tr.spec.audiences,
            Some(vec![HARNESS_RELAY_AUDIENCE.to_string()]),
        );
    }

    #[test]
    fn audience_constants_are_distinct() {
        // Leak-prevention invariant: every audience pair must be distinct.
        // If a refactor accidentally aliases two of them, a stolen token of
        // one consumer would unlock the other.
        let all = [
            HARNESS_HANGAR_AUDIENCE,
            HARNESS_AIRLOCK_AUDIENCE,
            HARNESS_MAINFRAME_AUDIENCE,
            LLM_HANGAR_AUDIENCE,
            CHAMBER_AIRLOCK_AUDIENCE,
            HANGAR_HARNESS_AUDIENCE,
            RELAY_HANGAR_AUDIENCE,
            HARNESS_RELAY_AUDIENCE,
            HANGAR_RELAY_AUDIENCE,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "audiences {i} and {j} must differ");
            }
        }
    }

    #[test]
    fn build_token_review_includes_token() {
        let tr = build_token_review("the-token", HARNESS_HANGAR_AUDIENCE);
        assert_eq!(tr.spec.token, Some("the-token".to_string()));
    }
}
