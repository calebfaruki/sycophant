use async_trait::async_trait;
use k8s_openapi::api::authentication::v1::{TokenReview, TokenReviewSpec};
use k8s_openapi::api::authorization::v1::{
    ResourceAttributes, SubjectAccessReview, SubjectAccessReviewSpec,
};
use kube::api::PostParams;
use kube::{Api, Client};
use tonic::{Request, Status};

#[async_trait]
pub trait TokenVerifier: Send + Sync {
    /// Check `token` and return the caller's raw TokenReview username.
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

    /// The audience every review this verifier submits is pinned to.
    pub fn audience(&self) -> &str {
        &self.audience
    }
}

/// Build a TokenReview for `token`, pinned to `audience` when one is given.
/// A person token rides on an unpinned review: the apiserver's OIDC
/// authenticator enforces the issuer audience at authentication time.
///
/// Pure function — separated from the API call so the spec construction
/// (token + audiences) is unit-testable without a kube client.
fn build_token_review(token: &str, audience: Option<&str>) -> TokenReview {
    TokenReview {
        metadata: Default::default(),
        spec: TokenReviewSpec {
            token: Some(token.to_string()),
            audiences: audience.map(|a| vec![a.to_string()]),
        },
        status: None,
    }
}

#[async_trait]
impl TokenVerifier for K8sTokenVerifier {
    async fn verify_token(&self, token: &str) -> Result<String, Status> {
        review_token_username(&self.client, token, Some(&self.audience)).await
    }
}

/// Map an authenticated ServiceAccount username to its agent name.
///
/// Pure function — separated from the API call so the SA-name mapping is
/// unit-testable without a kube client.
#[allow(clippy::result_large_err)]
pub fn agent_from_username(username: &str) -> Result<String, Status> {
    let sa_name = serviceaccount_from_username(username)
        .map(|(_, sa)| sa)
        .ok_or_else(|| Status::permission_denied("caller is not a ServiceAccount"))?;

    parse_agent_from_sa(sa_name)
        .map(|agent| agent.to_string())
        .ok_or_else(|| {
            Status::permission_denied(format!("ServiceAccount {sa_name} is not an agent SA"))
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

/// The apiserver's username prefix for a ServiceAccount token:
/// `system:serviceaccount:<namespace>:<name>`.
const SERVICEACCOUNT_USERNAME_PREFIX: &str = "system:serviceaccount:";

/// Split a ServiceAccount username `system:serviceaccount:<ns>:<name>` into
/// `(namespace, name)`. `None` when `username` is not a ServiceAccount.
pub fn serviceaccount_from_username(username: &str) -> Option<(&str, &str)> {
    username
        .strip_prefix(SERVICEACCOUNT_USERNAME_PREFIX)
        .and_then(|rest| rest.split_once(':'))
}

/// Every agent object name starts with this prefix. The harness pod's
/// ServiceAccount is `ag-<agent>`.
pub const AGENT_PREFIX: &str = "ag-";

/// Suffix on the harness-created tool-job pod's ServiceAccount,
/// `ag-<agent>-unprivileged`. Both identities map to the same <agent>.
pub const UNPRIVILEGED_SA_SUFFIX: &str = "-unprivileged";

/// The tool-job pod's ServiceAccount name for `agent`.
pub fn unprivileged_sa(agent: &str) -> String {
    format!("{AGENT_PREFIX}{agent}{UNPRIVILEGED_SA_SUFFIX}")
}

pub fn parse_agent_from_sa(sa_name: &str) -> Option<&str> {
    let rest = sa_name.strip_prefix(AGENT_PREFIX)?;
    let agent = rest.strip_suffix(UNPRIVILEGED_SA_SUFFIX).unwrap_or(rest);
    if agent.is_empty() {
        None
    } else {
        Some(agent)
    }
}

/// Path to the SA token mounted into harness pods and in-cluster
/// capability jobs. Harness pods mount a custom-audience projected token; the
/// broad pod VAP component-gates the kube-apiserver audience away. In-cluster
/// jobs mount a token at the kubelet-default path; the audience differs, the
/// path doesn't.
pub const SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

/// Audience for the tool-job auth token, mounted into each tool job as the
/// `tool-job-auth` projected volume (see `crates/harness/src/job.rs`). Each
/// audience is distinct per consumer, so a token minted for one audience cannot
/// be replayed against another. Naming convention:
/// `<sender>.<recipient>.sycophant.md`.
pub const TOOL_TOOLSET_AUDIENCE: &str = "tool.toolset.sycophant.md";

/// Audience for the relay-controller pod → harness pods. Relay
/// forwards external client calls to the harness. NetworkPolicy
/// admits relay-ctrl alone to the harness's port; the harness checks
/// no token.
pub const RELAY_HARNESS_AUDIENCE: &str = "relay.harness.sycophant.md";

/// Audience for the harness pod → relay-controller internal
/// listener (Subscribe, the server-request methods, and the `Deliver*` pushes).
/// Relay pins this audience on TokenReview for harness-bound
/// internal methods.
pub const HARNESS_RELAY_AUDIENCE: &str = "harness.relay.sycophant.md";

/// Audience for the token a parent agent's harness presents when it attaches
/// to a dispatch's `DispatchLink`.
pub const HARNESS_DISPATCH_AUDIENCE: &str = "harness.dispatch.sycophant.md";

/// Tonic interceptor that injects an SA token as a `Bearer <token>`
/// Authorization header on every outgoing request. The token is
/// re-read from `token_path` on each call so kubelet rotation is
/// observed.
///
/// Parameterized over path so a single process can wield distinct
/// audience-bound tokens against different verifiers: capability jobs use the
/// kubelet-default path via `default_path()`; other callers pass the path of
/// their own audience-bound projected token.
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

    /// The kubelet-default mount path. In-cluster capability jobs that mount their
    /// projected token at `/var/run/secrets/kubernetes.io/serviceaccount`
    /// construct via this helper.
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

/// On-disk mount path for the harness's relay-audience SA token.
/// The chart's harness Deployment mounts the `harness-relay-auth`
/// projected volume here. Used by the harness to dial the gateway's
/// internal listener (Subscribe + the channel-stream server-request methods).
pub const HARNESS_RELAY_TOKEN_PATH: &str = "/var/run/secrets/harness/relay/token";

/// On-disk mount path for the relay-controller's harness-audience
/// SA token. The chart's relay-ctrl Deployment mounts a projected
/// volume here. Used by relay to dial per-agent harness pods
/// when forwarding HarnessControl calls.
pub const RELAY_HARNESS_TOKEN_PATH: &str = "/var/run/secrets/relay/harness/token";

/// Audience a channel's SA token is minted for and the relay pins on
/// TokenReview, per the `<sender>.<recipient>.sycophant.md` convention.
pub const CHANNEL_RELAY_AUDIENCE: &str = "channel.relay.sycophant.md";

/// Build the SubjectAccessReview checking `subject` for `get` on the named
/// agent's harness Service in the relay's own namespace. The namespace is
/// the relay's, never a caller-supplied one, so a request naming an agent
/// outside the tenant namespace finds a Service the caller holds no grant on.
///
/// Pure function — separated from the API call so the spec construction is
/// unit-testable without a kube client.
pub fn build_subject_access_review(
    subject: &str,
    agent: &str,
    namespace: &str,
) -> SubjectAccessReview {
    SubjectAccessReview {
        metadata: Default::default(),
        spec: SubjectAccessReviewSpec {
            extra: None,
            groups: None,
            non_resource_attributes: None,
            resource_attributes: Some(ResourceAttributes {
                group: Some(String::new()),
                name: Some(format!("{AGENT_PREFIX}{agent}")),
                namespace: Some(namespace.to_string()),
                resource: Some("services".to_string()),
                subresource: None,
                verb: Some("get".to_string()),
                version: None,
                field_selector: None,
                label_selector: None,
            }),
            uid: None,
            user: Some(subject.to_string()),
        },
        status: None,
    }
}

/// Submit a TokenReview for `token` and return the raw authenticated username,
/// without the SA-name→agent mapping.
///
/// With `audience` set the review is pinned to it and the apiserver must echo
/// it back in `status.audiences`, because a non-audience-aware authenticator
/// could otherwise answer `authenticated=true` for a token minted elsewhere.
/// With `None` (the person-token path) no audience is pinned.
pub async fn review_token_username(
    client: &Client,
    token: &str,
    audience: Option<&str>,
) -> Result<String, Status> {
    let tr = build_token_review(token, audience);
    let token_reviews: Api<TokenReview> = Api::all(client.clone());
    let result = token_reviews
        .create(&PostParams::default(), &tr)
        .await
        .map_err(|e| Status::internal(format!("TokenReview API error: {e}")))?;

    username_from_review(result, audience)
}

/// Extract the authenticated username from a completed `TokenReview`. With
/// `expected_audience` set, the audience echo is required before trusting the
/// verdict.
///
/// Pure function over the review payload — separated from the API call so the
/// authentication decision logic is unit-testable without a kube client.
#[allow(clippy::result_large_err)]
fn username_from_review(
    review: TokenReview,
    expected_audience: Option<&str>,
) -> Result<String, Status> {
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
    if let Some(expected) = expected_audience {
        let audience_ok = status
            .audiences
            .as_ref()
            .is_some_and(|auds| auds.iter().any(|a| a == expected));
        if !audience_ok {
            return Err(Status::permission_denied("audience echo mismatch"));
        }
    }

    let username = status
        .user
        .and_then(|u| u.username)
        .ok_or_else(|| Status::internal("no username in TokenReview"))?;
    if username.is_empty() {
        return Err(Status::permission_denied(
            "authenticated token carries no username",
        ));
    }
    Ok(username)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent_resolves_harness_and_unprivileged_and_rejects_bare() {
        assert_eq!(parse_agent_from_sa("ag-hello-world"), Some("hello-world"));
        assert_eq!(
            parse_agent_from_sa("ag-hello-world-unprivileged"),
            Some("hello-world")
        );
        assert_eq!(parse_agent_from_sa("ag-"), None);
        assert_eq!(parse_agent_from_sa("ag--unprivileged"), None);
        assert_eq!(parse_agent_from_sa("default"), None);
    }

    #[test]
    fn extract_bearer_only_accepts_a_bearer_authorization_header() {
        let mut req = Request::new(());
        req.metadata_mut()
            .insert("authorization", "Bearer test-token".parse().unwrap());
        assert_eq!(extract_bearer_token(&req).unwrap(), "test-token");

        let req = Request::new(());
        let err = extract_bearer_token(&req).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);

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
    fn pinned_review_unauthenticated_returns_permission_denied() {
        let review = review_with(
            Some(false),
            Some("system:serviceaccount:ns:ag-alice"),
            Some(vec![TEST_AUDIENCE]),
        );
        let err = username_from_review(review, Some(TEST_AUDIENCE)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("invalid token"));
    }

    #[test]
    fn pinned_review_missing_authenticated_field_denies() {
        // `authenticated: None` defaults to false via `unwrap_or(false)`.
        let review = review_with(
            None,
            Some("system:serviceaccount:ns:ag-alice"),
            Some(vec![TEST_AUDIENCE]),
        );
        let err = username_from_review(review, Some(TEST_AUDIENCE)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    /// The full username mapping, not only the prefix-strip helper.
    #[test]
    fn agent_from_username_unprivileged_subject_resolves_agent() {
        let agent =
            agent_from_username("system:serviceaccount:ns:ag-hello-world-unprivileged").unwrap();
        assert_eq!(agent, "hello-world");
    }

    #[test]
    fn agent_from_username_non_agent_sa_denies() {
        let err = agent_from_username("system:serviceaccount:ns:default").unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("not an agent SA"));
    }

    #[test]
    fn pinned_review_no_status_is_internal_error() {
        let review = TokenReview {
            metadata: Default::default(),
            spec: TokenReviewSpec::default(),
            status: None,
        };
        let err = username_from_review(review, Some(TEST_AUDIENCE)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
    }

    #[test]
    fn pinned_review_rejects_missing_audiences_echo() {
        let review = review_with(
            Some(true),
            Some("system:serviceaccount:ns:ag-hello-world"),
            None,
        );
        let err = username_from_review(review, Some(TEST_AUDIENCE)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("audience echo mismatch"));
    }

    #[test]
    fn pinned_review_rejects_audiences_echo_without_requested() {
        let review = review_with(
            Some(true),
            Some("system:serviceaccount:ns:ag-hello-world"),
            Some(vec!["unrelated.sycophant.md", "other.example.com"]),
        );
        let err = username_from_review(review, Some(TEST_AUDIENCE)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("audience echo mismatch"));
    }

    #[test]
    fn pinned_review_accepts_audiences_echo_containing_requested() {
        // Multi-audience echo with the requested audience among others — must
        // succeed: any listed audience matches, not strict equality.
        let review = review_with(
            Some(true),
            Some("system:serviceaccount:ns:ag-hello-world"),
            Some(vec!["other.example.com", TEST_AUDIENCE]),
        );
        let username = username_from_review(review, Some(TEST_AUDIENCE)).unwrap();
        assert_eq!(username, "system:serviceaccount:ns:ag-hello-world");
    }

    #[test]
    fn build_token_review_carries_the_token_and_its_single_audience() {
        let tr = build_token_review("the-token", Some(TOOL_TOOLSET_AUDIENCE));
        assert_eq!(
            tr.spec.audiences,
            Some(vec![TOOL_TOOLSET_AUDIENCE.to_string()]),
            "TokenReviewSpec.audiences must carry the configured audience so \
             kube-apiserver rejects tokens minted for other audiences"
        );
        assert_eq!(tr.spec.token, Some("the-token".to_string()));
    }

    #[test]
    fn audience_constants_are_distinct() {
        // Leak-prevention invariant: every audience pair must be distinct.
        // If a refactor accidentally aliases two of them, a stolen token of
        // one consumer would unlock the other.
        let all = [
            TOOL_TOOLSET_AUDIENCE,
            RELAY_HARNESS_AUDIENCE,
            HARNESS_RELAY_AUDIENCE,
            CHANNEL_RELAY_AUDIENCE,
            HARNESS_DISPATCH_AUDIENCE,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "audiences {i} and {j} must differ");
            }
        }
    }

    // --- Person-token reviews and the username they yield ---

    #[test]
    fn subject_review_targets_the_service_in_the_relay_namespace() {
        let sar =
            build_subject_access_review("https://issuer.example#alice", "beta", "tenant-acme");
        assert_eq!(
            sar.spec.user.as_deref(),
            Some("https://issuer.example#alice")
        );
        let ra = sar.spec.resource_attributes.expect("must carry attributes");
        assert_eq!(ra.verb.as_deref(), Some("get"));
        assert_eq!(ra.resource.as_deref(), Some("services"));
        assert_eq!(ra.name.as_deref(), Some("ag-beta"));
        assert_eq!(ra.namespace.as_deref(), Some("tenant-acme"));
        // Identity maps as username only: no groups, no uid, no extra.
        assert!(sar.spec.groups.is_none());
        assert!(sar.spec.uid.is_none());
        assert!(sar.spec.extra.is_none());
    }

    #[test]
    fn unpinned_review_carries_no_audiences() {
        let tr = build_token_review("the-token", None);
        assert_eq!(tr.spec.token, Some("the-token".to_string()));
        assert!(
            tr.spec.audiences.is_none(),
            "the person-token review pins no audience"
        );
    }

    #[test]
    fn username_from_review_returns_the_raw_username() {
        let review = review_with(Some(true), Some("https://issuer.example#alice"), None);
        assert_eq!(
            username_from_review(review, None).unwrap(),
            "https://issuer.example#alice"
        );
    }

    #[test]
    fn username_from_review_empty_username_denies() {
        let review = review_with(Some(true), Some(""), None);
        let err = username_from_review(review, None).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }
}
