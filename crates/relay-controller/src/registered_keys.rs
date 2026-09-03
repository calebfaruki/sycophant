//! The relay-owned registered-key store: one device public key per
//! operator-verified grant row, keyed by that row.
//!
//! The Secret is what lets a redeemed device survive a relay pod roll. It is
//! also the narrower of the two sources — a key whose grant row has since
//! been deleted is never reinstalled, or revocation would undo itself at the
//! next restart.
//!
//! The relay is the sole writer. RBAC scopes `update`/`patch` on Secrets to
//! this name alone, so no other Secret in the namespace is writable.

use std::collections::BTreeMap;

use base64::Engine as _;
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Api, ObjectMeta, Patch, PatchParams, PostParams};
use kube::Client as KubeClient;
use p256::ecdsa::VerifyingKey;
use shared::client_signature::{ClientRegistration, ClientSignatureVerifier};
use tonic::Status;

use crate::grants::RelayGrants;

/// Relay-owned Secret holding registered public keys, one entry per grant
/// row. Named in the relay Role's `resourceNames` and in the cluster's
/// Secret-name allowlist VAP.
pub const REGISTERED_KEYS_SECRET_NAME: &str = "relay-registered-keys";

fn decode_sec1_b64(wire: &str) -> Option<VerifyingKey> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(wire.trim())
        .ok()?;
    VerifyingKey::from_sec1_bytes(&raw).ok()
}

/// Read the registered-keys Secret and install every key whose grant row is
/// still live. Returns how many were installed. A missing Secret is first
/// start, not an error.
pub async fn load_into_verifier(
    client: &KubeClient,
    namespace: &str,
    grants: &RelayGrants,
    verifier: &ClientSignatureVerifier,
) -> Result<usize, String> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = api
        .get_opt(REGISTERED_KEYS_SECRET_NAME)
        .await
        .map_err(|e| format!("reading {REGISTERED_KEYS_SECRET_NAME}: {e}"))?;
    let Some(secret) = secret else {
        tracing::info!(
            secret = REGISTERED_KEYS_SECRET_NAME,
            "no registered-keys Secret yet; starting with an empty verifier"
        );
        return Ok(0);
    };

    let registrations = verifier.registrations();
    let mut map = registrations.write().await;
    let mut installed = 0usize;

    for (row_key, value) in secret.data.iter().flatten() {
        let Some(row) = grants.get(row_key) else {
            tracing::info!(
                row = %row_key,
                "registered key has no live grant row; not installing"
            );
            continue;
        };
        let Ok(wire) = std::str::from_utf8(&value.0) else {
            tracing::warn!(row = %row_key, "registered key is not UTF-8; skipping");
            continue;
        };
        let Some(verifying_key) = decode_sec1_b64(wire) else {
            tracing::warn!(row = %row_key, "registered key is not a SEC1 P-256 point; skipping");
            continue;
        };
        map.insert(
            row_key.clone(),
            ClientRegistration {
                verifying_key,
                workspace: row.workspace.clone(),
            },
        );
        installed += 1;
    }

    tracing::info!(installed, "rebuilt signature verifier from registered keys");
    Ok(installed)
}

/// Persist one row's device public key. Merge-patches so other rows' keys
/// are untouched; creates the Secret on first registration.
pub async fn register_key(
    client: &KubeClient,
    namespace: &str,
    row_key: &str,
    public_key_sec1: &[u8],
) -> Result<(), Status> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(public_key_sec1);
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);

    let patch = serde_json::json!({ "stringData": { row_key: encoded } });
    match api
        .patch(
            REGISTERED_KEYS_SECRET_NAME,
            &PatchParams::default(),
            &Patch::Merge(&patch),
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(e)) if e.code == 404 => {
            let secret = build_secret(namespace, row_key, &encoded);
            api.create(&PostParams::default(), &secret)
                .await
                .map(|_| ())
                .map_err(|e| Status::internal(format!("creating registered-keys Secret: {e}")))
        }
        Err(e) => Err(Status::internal(format!(
            "recording registered key for {row_key}: {e}"
        ))),
    }
}

fn build_secret(namespace: &str, row_key: &str, encoded: &str) -> Secret {
    let mut string_data = BTreeMap::new();
    string_data.insert(row_key.to_string(), encoded.to_string());
    Secret {
        metadata: ObjectMeta {
            name: Some(REGISTERED_KEYS_SECRET_NAME.into()),
            namespace: Some(namespace.into()),
            ..Default::default()
        },
        string_data: Some(string_data),
        ..Default::default()
    }
}
