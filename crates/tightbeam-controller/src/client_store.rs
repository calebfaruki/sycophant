//! Storage interface for Client CR access. The `ClientStore` trait
//! decouples the `redeem_for_client` business logic from `kube::Api`
//! so unit tests can inject a stub that returns canned Client states
//! — covering the four branches (invalid input, not-found,
//! already-enrolled, success) without a real cluster.
//!
//! The production impl `KubeClientStore` wraps `kube::Api<Client>` and
//! maps kube errors to `tonic::Status` at the boundary.

use async_trait::async_trait;
use kube::api::{Api, Patch, PatchParams};
use kube::Client as KubeClient;
use shared::auth::EnrollmentClaims;
use tightbeam_proto::RedeemEnrollmentResponse;
use tonic::Status;

use crate::client_watcher::{build_redeem_patch, FIELD_MANAGER};
use crate::crd::Client;

#[async_trait]
pub trait ClientStore: Send + Sync {
    /// Fetch a Client CR by name. `Ok(None)` means 404; `Err` for
    /// other failures.
    async fn get(&self, name: &str) -> Result<Option<Client>, Status>;
    /// SSA-patch the Client's status subresource using `FIELD_MANAGER`.
    async fn patch_status(&self, name: &str, patch: &serde_json::Value) -> Result<(), Status>;
}

pub struct KubeClientStore {
    api: Api<Client>,
}

impl KubeClientStore {
    pub fn new(client: KubeClient, namespace: &str) -> Self {
        Self {
            api: Api::namespaced(client, namespace),
        }
    }
}

/// Map a kube fetch result into the `ClientStore::get` contract:
/// `Ok(Some)` for found, `Ok(None)` for 404, `Err(Status::internal)`
/// for any other kube failure. Extracted so the 404-vs-other branch is
/// covered by a pure unit test.
#[allow(clippy::result_large_err)] // tonic::Status is the gRPC-shaped error this layer returns
fn map_kube_get(result: Result<Client, kube::Error>) -> Result<Option<Client>, Status> {
    match result {
        Ok(c) => Ok(Some(c)),
        Err(kube::Error::Api(api_err)) if api_err.code == 404 => Ok(None),
        Err(e) => Err(Status::internal(format!("kube get failed: {e}"))),
    }
}

#[async_trait]
impl ClientStore for KubeClientStore {
    async fn get(&self, name: &str) -> Result<Option<Client>, Status> {
        map_kube_get(self.api.get(name).await)
    }

    async fn patch_status(&self, name: &str, patch: &serde_json::Value) -> Result<(), Status> {
        let pp = PatchParams::apply(FIELD_MANAGER).force();
        self.api
            .patch_status(name, &pp, &Patch::Apply(patch))
            .await
            .map_err(|e| Status::internal(format!("patch_status failed: {e}")))?;
        Ok(())
    }
}

/// Redeem an enrollment code: validate the supplied public key,
/// enforce the single-use guard (ADR 013 Q8), patch the Client's
/// status with the registered key, and build the response. All kube
/// I/O routes through `store` so tests can inject a stub.
pub async fn redeem_for_client(
    store: &dyn ClientStore,
    claims: &EnrollmentClaims,
    public_key: &[u8],
) -> Result<RedeemEnrollmentResponse, Status> {
    use base64::Engine;

    // SEC1 validation. Empty bytes, wrong length, or off-curve all
    // collapse to a uniform InvalidArgument — the caller has no
    // business distinguishing.
    p256::ecdsa::VerifyingKey::from_sec1_bytes(public_key)
        .map_err(|_| Status::invalid_argument("public_key is not a valid P-256 SEC1 point"))?;

    let current = store
        .get(&claims.device_name)
        .await?
        .ok_or_else(|| Status::not_found(format!("client {} not found", claims.device_name)))?;

    // Single-use guard (ADR 013 Q8): once `status.publicKey` is set,
    // the Client is enrolled. Operator must clear it via
    // `kubectl patch client foo --subresource=status -p
    // '{"status":{"publicKey":null}}'` before a fresh redemption can
    // succeed. Defends against a leaked enrollment code becoming a
    // permanent device hijack.
    if current.status.and_then(|s| s.public_key).is_some() {
        return Err(Status::failed_precondition(
            "client already enrolled; rotate via status patch first",
        ));
    }

    let public_key_b64 = base64::engine::general_purpose::STANDARD.encode(public_key);
    let enrolled_at_rfc3339 = chrono::Utc::now().to_rfc3339();
    let patch = build_redeem_patch(&claims.device_name, &public_key_b64, &enrolled_at_rfc3339);
    store.patch_status(&claims.device_name, &patch).await?;

    let enrolled_at_unix = chrono::DateTime::parse_from_rfc3339(&enrolled_at_rfc3339)
        .map(|dt| dt.timestamp())
        .unwrap_or_default();

    Ok(RedeemEnrollmentResponse {
        client_name: claims.device_name.clone(),
        enrolled_at: enrolled_at_unix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::ObjectMeta;
    use p256::ecdsa::SigningKey as P256SigningKey;
    use p256::elliptic_curve::rand_core::OsRng;
    use std::sync::Mutex;

    use crate::crd::{ClientSpec, ClientStatus};

    /// In-memory stub. `fetched` returns whatever was set; `patches`
    /// records every patch_status call. Construct via `ok(...)` for
    /// the happy-fetch path or `get_error(...)` to simulate a kube
    /// failure on the read.
    struct StubClientStore {
        fetched: Mutex<Option<Client>>,
        get_error: Mutex<Option<Status>>,
        patches: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl StubClientStore {
        fn ok(fetched: Option<Client>) -> Self {
            Self {
                fetched: Mutex::new(fetched),
                get_error: Mutex::new(None),
                patches: Mutex::new(Vec::new()),
            }
        }

        fn get_error(status: Status) -> Self {
            Self {
                fetched: Mutex::new(None),
                get_error: Mutex::new(Some(status)),
                patches: Mutex::new(Vec::new()),
            }
        }

        fn patches(&self) -> Vec<(String, serde_json::Value)> {
            self.patches.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ClientStore for StubClientStore {
        async fn get(&self, _name: &str) -> Result<Option<Client>, Status> {
            if let Some(s) = self.get_error.lock().unwrap().as_ref() {
                return Err(Status::new(s.code(), s.message().to_string()));
            }
            Ok(self.fetched.lock().unwrap().clone())
        }

        async fn patch_status(&self, name: &str, patch: &serde_json::Value) -> Result<(), Status> {
            self.patches
                .lock()
                .unwrap()
                .push((name.to_string(), patch.clone()));
            Ok(())
        }
    }

    fn fresh_sec1() -> Vec<u8> {
        let sk = P256SigningKey::random(&mut OsRng);
        sk.verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec()
    }

    fn claims(device_name: &str) -> EnrollmentClaims {
        EnrollmentClaims {
            workspace: "hello-world".into(),
            device_name: device_name.into(),
            code_id: "code-uuid".into(),
            exp: 0,
        }
    }

    fn unregistered_client(name: &str) -> Client {
        Client {
            metadata: ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            spec: ClientSpec {
                workspaces: vec!["hello-world".into()],
            },
            status: None,
        }
    }

    fn already_enrolled_client(name: &str) -> Client {
        Client {
            metadata: ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            spec: ClientSpec {
                workspaces: vec!["hello-world".into()],
            },
            status: Some(ClientStatus {
                public_key: Some("previously-registered".into()),
                ..Default::default()
            }),
        }
    }

    #[tokio::test]
    async fn redeem_with_invalid_sec1_public_key_returns_invalid_argument() {
        // Garbage bytes fail SEC1 validation before any kube I/O.
        // Empty input takes the same path (length check first).
        let store = StubClientStore::ok(Some(unregistered_client("alpha")));
        let err = redeem_for_client(&store, &claims("alpha"), b"not a key")
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            store.patches().is_empty(),
            "no patch should issue when SEC1 validation rejects"
        );
    }

    #[tokio::test]
    async fn redeem_with_empty_public_key_returns_invalid_argument() {
        // Empty input takes the same SEC1 length check; pins behavior so a
        // future refactor that splits empty vs invalid messages still
        // returns the same status code.
        let store = StubClientStore::ok(Some(unregistered_client("alpha")));
        let err = redeem_for_client(&store, &claims("alpha"), &[])
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(store.patches().is_empty());
    }

    #[tokio::test]
    async fn redeem_with_missing_client_cr_returns_not_found() {
        let store = StubClientStore::ok(None);
        let err = redeem_for_client(&store, &claims("ghost"), &fresh_sec1())
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
        assert!(store.patches().is_empty());
    }

    #[tokio::test]
    async fn redeem_with_already_enrolled_client_returns_failed_precondition() {
        // ADR 013 Q8 — the single-use guard. Defends against a leaked
        // enrollment code becoming a permanent device hijack.
        let store = StubClientStore::ok(Some(already_enrolled_client("alpha")));
        let err = redeem_for_client(&store, &claims("alpha"), &fresh_sec1())
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert!(
            store.patches().is_empty(),
            "single-use guard must not issue a patch on the second redemption"
        );
    }

    #[tokio::test]
    async fn redeem_with_unregistered_client_persists_public_key_and_clears_code() {
        use base64::Engine;
        let store = StubClientStore::ok(Some(unregistered_client("alpha")));
        let sec1 = fresh_sec1();
        let resp = redeem_for_client(&store, &claims("alpha"), &sec1)
            .await
            .unwrap();
        assert_eq!(resp.client_name, "alpha");
        assert!(resp.enrolled_at > 0, "enrolled_at must be a real timestamp");
        let patches = store.patches();
        assert_eq!(patches.len(), 1, "exactly one patch issued");
        let (name, patch) = &patches[0];
        assert_eq!(name, "alpha");
        assert_eq!(
            patch["status"]["publicKey"].as_str().unwrap(),
            base64::engine::general_purpose::STANDARD.encode(&sec1),
        );
        // build_redeem_patch omits enrollmentCode so SSA clears it on
        // apply — pin the omission here too.
        assert!(
            patch["status"].get("enrollmentCode").is_none(),
            "patch must NOT carry enrollmentCode (SSA clears via omission)"
        );
    }

    #[tokio::test]
    async fn redeem_propagates_store_get_error() {
        let store = StubClientStore::get_error(Status::internal("kube down"));
        let err = redeem_for_client(&store, &claims("alpha"), &fresh_sec1())
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
        assert!(store.patches().is_empty());
    }

    fn api_error(code: u16) -> kube::Error {
        kube::Error::Api(Box::new(kube::core::Status {
            status: None,
            code,
            message: "test".into(),
            metadata: None,
            reason: "test".into(),
            details: None,
        }))
    }

    fn dummy_client() -> Client {
        Client {
            metadata: ObjectMeta {
                name: Some("alpha".into()),
                ..Default::default()
            },
            spec: ClientSpec {
                workspaces: vec!["hello-world".into()],
            },
            status: None,
        }
    }

    #[test]
    fn map_kube_get_ok_returns_some() {
        let r = map_kube_get(Ok(dummy_client())).unwrap();
        assert!(r.is_some());
    }

    #[test]
    fn map_kube_get_404_returns_none() {
        let r = map_kube_get(Err(api_error(404))).unwrap();
        assert!(r.is_none(), "404 must map to Ok(None), got {:?}", r);
    }

    #[test]
    fn map_kube_get_500_returns_internal_status() {
        // Defends the 404-vs-other branch: only 404 → None.
        let err = map_kube_get(Err(api_error(500))).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
    }

    #[test]
    fn map_kube_get_403_returns_internal_status() {
        // Same branch from a different non-404 code, defends `==` vs `>=`
        // / `!=` mutations on the 404 check.
        let err = map_kube_get(Err(api_error(403))).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
    }
}
