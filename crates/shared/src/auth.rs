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
}
