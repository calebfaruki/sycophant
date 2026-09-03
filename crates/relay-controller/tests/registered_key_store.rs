//! The relay's registered-key store is a relay-owned Secret, and the
//! signature verifier is rebuilt from it on restart.
//!
//! The Secret is the only reason a redeemed device survives a relay pod roll.
//! It also has to be the *narrower* of the two sources: a key whose grant row
//! has since been deleted must not come back to life on restart, or the
//! "revoked within seconds, without a pod restart" promise is undone by the
//! next restart.
//!
//! The contract these tests pin:
//!
//! ```ignore
//! // relay_controller::registered_keys
//! pub const REGISTERED_KEYS_SECRET_NAME: &str = "relay-registered-keys";
//! pub async fn load_into_verifier(
//!     client: &kube::Client,
//!     namespace: &str,
//!     grants: &RelayGrants,
//!     verifier: &ClientSignatureVerifier,
//! ) -> Result<usize, String>;
//!
//! // shared::client_signature, row-bound
//! pub struct ClientRegistration {
//!     pub verifying_key: p256::ecdsa::VerifyingKey,
//!     pub workspace: String,
//! }
//! ```
//!
//! The registration map stays keyed by `kid`, and the `kid` is the grant row
//! key: the redemption response's client name *is* the row key.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use http_body_util::BodyExt;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::ByteString;
use kube::client::Body as KubeBody;
use p256::ecdsa::{SigningKey, VerifyingKey};
use p256::elliptic_curve::rand_core::OsRng;

use relay_controller::grants::{apply_delivery, RelayGrants};
use relay_controller::registered_keys::{load_into_verifier, REGISTERED_KEYS_SECRET_NAME};
use shared::client_signature::ClientSignatureVerifier;

const NAMESPACE: &str = "tenant";

fn fresh_key() -> (VerifyingKey, String) {
    let vk = *SigningKey::random(&mut OsRng).verifying_key();
    let sec1 = vk.to_encoded_point(false);
    let encoded = base64::engine::general_purpose::STANDARD.encode(sec1.as_bytes());
    (vk, encoded)
}

/// Serves the registered-keys Secret on GET and 404s everything else, so a
/// rebuild that reaches for any other object fails loudly instead of silently.
fn kube_client_serving(secret: Option<Secret>, gets: Arc<Mutex<Vec<String>>>) -> kube::Client {
    let secret = Arc::new(secret);
    let svc = tower::service_fn(move |req: http::Request<KubeBody>| {
        let secret = secret.clone();
        let gets = gets.clone();
        async move {
            let (parts, body) = req.into_parts();
            let _ = body.collect().await;
            let path = parts.uri.path().to_string();
            gets.lock().unwrap().push(path.clone());

            let serves_secret = parts.method == http::Method::GET
                && path.ends_with(&format!("/secrets/{REGISTERED_KEYS_SECRET_NAME}"));

            let resp = match (serves_secret, secret.as_ref()) {
                (true, Some(s)) => http::Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(KubeBody::from(serde_json::to_vec(s).unwrap()))
                    .unwrap(),
                _ => http::Response::builder()
                    .status(404)
                    .header("content-type", "application/json")
                    .body(KubeBody::from(
                        br#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":404,"reason":"NotFound"}"#
                            .to_vec(),
                    ))
                    .unwrap(),
            };
            Ok::<_, std::convert::Infallible>(resp)
        }
    });
    kube::Client::new(svc, NAMESPACE)
}

fn registered_keys_secret(entries: &[(&str, &str)]) -> Secret {
    let mut data = BTreeMap::new();
    for (row, b64) in entries {
        data.insert((*row).to_string(), ByteString((*b64).as_bytes().to_vec()));
    }
    Secret {
        metadata: ObjectMeta {
            name: Some(REGISTERED_KEYS_SECRET_NAME.into()),
            namespace: Some(NAMESPACE.into()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    }
}

fn grants(rows: &[(&str, &str, &str, &str)]) -> RelayGrants {
    let data = rows
        .iter()
        .map(|(key, channel, identity, workspace)| {
            (
                (*key).to_string(),
                format!("channel: {channel}\nidentity: {identity}\nworkspace: {workspace}\n"),
            )
        })
        .collect::<BTreeMap<String, String>>();
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some("relay-grants".into()),
            namespace: Some(NAMESPACE.into()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };
    let (table, errors) = apply_delivery(&cm);
    assert!(errors.is_empty(), "fixture grants must all parse");
    table
}

fn verifier() -> ClientSignatureVerifier {
    ClientSignatureVerifier::new(Duration::from_secs(300))
}

/// Keep the registered keys in the verifier's in-memory map only and every
/// enrolled device is silently deauthenticated by the next relay restart, with
/// no error anywhere. The Secret exists for exactly this, so a rebuild that
/// does not restore the key is the Secret doing nothing.
#[tokio::test]
async fn the_verifier_is_rebuilt_from_the_registered_keys_secret_on_restart() {
    let (phone_vk, phone_b64) = fresh_key();
    let (laptop_vk, laptop_b64) = fresh_key();
    let secret =
        registered_keys_secret(&[("caleb-phone", &phone_b64), ("caleb-laptop", &laptop_b64)]);
    let table = grants(&[
        ("caleb-phone", "app", "kJ8f2QwXnR4tYv6b", "family"),
        ("caleb-laptop", "app", "pQ3z7NmBc1dLe5wR", "family"),
    ]);

    let v = verifier();
    let loaded = load_into_verifier(
        &kube_client_serving(Some(secret), Arc::new(Mutex::new(Vec::<String>::new()))),
        NAMESPACE,
        &table,
        &v,
    )
    .await
    .expect("rebuild must succeed when the Secret is present");
    assert_eq!(loaded, 2);

    let registrations = v.registrations();
    let map = registrations.read().await;

    let phone = map
        .get("caleb-phone")
        .expect("the phone's row must be registered under its row key");
    assert_eq!(phone.verifying_key, phone_vk);
    assert_eq!(phone.workspace, "family");

    let laptop = map
        .get("caleb-laptop")
        .expect("the laptop's row must be registered under its row key");
    assert_eq!(laptop.verifying_key, laptop_vk);
    assert_eq!(laptop.workspace, "family");
}

/// Revocation holds across a restart. The operator deleted `dad-telegram`'s row while the
/// relay was down; the key material is still in the Secret because the relay is
/// its sole writer and nothing garbage-collects it.
///
/// Materiality: rebuild from the Secret alone — the obvious implementation —
/// and a revoked identity walks back in at the next pod roll holding a key the
/// relay itself hands back to the verifier. That is a revocation that undoes
/// itself, and no warm-path test can see it.
#[tokio::test]
async fn a_key_whose_grant_row_was_removed_is_not_reinstalled() {
    let (_kept_vk, kept_b64) = fresh_key();
    let (_gone_vk, gone_b64) = fresh_key();
    let secret = registered_keys_secret(&[("caleb-phone", &kept_b64), ("dad-telegram", &gone_b64)]);
    // `dad-telegram` is absent from the live grants table.
    let table = grants(&[("caleb-phone", "app", "kJ8f2QwXnR4tYv6b", "family")]);

    let v = verifier();
    let loaded = load_into_verifier(
        &kube_client_serving(Some(secret), Arc::new(Mutex::new(Vec::<String>::new()))),
        NAMESPACE,
        &table,
        &v,
    )
    .await
    .expect("rebuild must succeed");
    assert_eq!(loaded, 1, "only the row that still exists is installed");

    let registrations = v.registrations();
    let map = registrations.read().await;
    assert!(map.contains_key("caleb-phone"));
    assert!(
        !map.contains_key("dad-telegram"),
        "a key whose row is gone must not be installed on restart"
    );
}

/// First start: the Secret does not exist yet. The relay must come up serving,
/// with an empty verifier, not crash-loop.
///
/// Materiality: treat 404 as fatal and a fresh tenant's relay never reaches
/// Ready, so nobody can ever redeem the first code.
#[tokio::test]
async fn a_missing_secret_rebuilds_to_an_empty_verifier_rather_than_failing() {
    let v = verifier();
    let loaded = load_into_verifier(
        &kube_client_serving(None, Arc::new(Mutex::new(Vec::<String>::new()))),
        NAMESPACE,
        &grants(&[("caleb-phone", "app", "kJ8f2QwXnR4tYv6b", "family")]),
        &v,
    )
    .await
    .expect("a missing registered-keys Secret is first-start, not an error");
    assert_eq!(loaded, 0);
    assert!(v.registrations().read().await.is_empty());
}

/// Read from the relay's side: the rebuild must touch no Secret but the
/// registered-keys one. The RBAC scopes every Secret verb by `resourceNames`
/// to that name, and this is the runtime half of that claim.
///
/// Materiality: read the namespace's Secrets from one `Api::list` and the
/// relay starts depending on access it must not have; the chart-layer test
/// (`sa-permission-bounds/relay-ctrl-grants-and-key-rbac`) would then be the
/// thing that breaks, at deploy time, in a live tenant.
#[tokio::test]
async fn the_rebuild_reads_only_the_registered_keys_secret() {
    let paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let (_vk, b64) = fresh_key();
    let _ = load_into_verifier(
        &kube_client_serving(
            Some(registered_keys_secret(&[("caleb-phone", &b64)])),
            paths.clone(),
        ),
        NAMESPACE,
        &grants(&[("caleb-phone", "app", "kJ8f2QwXnR4tYv6b", "family")]),
        &verifier(),
    )
    .await
    .expect("rebuild must succeed");

    let seen = paths.lock().unwrap().clone();
    assert!(
        !seen.is_empty(),
        "the rebuild must actually reach the apiserver"
    );
    for path in &seen {
        assert!(
            path.ends_with(&format!("/secrets/{REGISTERED_KEYS_SECRET_NAME}")),
            "the rebuild touched {path}; it may read only the registered-keys Secret"
        );
    }
}
