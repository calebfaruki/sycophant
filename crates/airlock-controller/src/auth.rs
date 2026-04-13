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

        let status = result
            .status
            .ok_or_else(|| Status::internal("no TokenReview status"))?;
        if !status.authenticated.unwrap_or(false) {
            return Err(Status::permission_denied("invalid token"));
        }

        let username = status
            .user
            .and_then(|u| u.username)
            .ok_or_else(|| Status::internal("no username in TokenReview"))?;

        // username format: system:serviceaccount:<ns>:<sa-name>
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
}
