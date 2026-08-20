//! `RedeemCode` is operator verification: the code *is* the row's identity,
//! possession of the string is the whole proof, and the row is spent once a
//! key is registered against it.
//!
//! The contract these tests pin:
//!
//! ```ignore
//! // proto-common
//! message RedeemCodeRequest  { string code = 1; bytes public_key = 2; }
//! message RedeemCodeResponse { string client_name = 1; int64 enrolled_at = 2; }
//!
//! // relay_controller::state
//! impl GatewayState { pub fn grants(&self) -> Arc<RwLock<GrantsTable>>; }
//!
//! // relay_controller::gateway, on the RelayGateway service
//! async fn redeem_code(&self, Request<RedeemCodeRequest>) -> Result<Response<RedeemCodeResponse>, Status>;
//! ```
//!
//! Three of these tests exploit one ordering fact: with `kube_client: None`,
//! anything decided before the registered-key store is reached surfaces as its
//! own status code, and anything that reaches the store surfaces as
//! `FailedPrecondition`. That is how "was this rejected, or did it get through?"
//! is observed without a cluster.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

use base64::Engine as _;
use http_body_util::BodyExt;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::client::Body as KubeBody;
use p256::ecdsa::{SigningKey, VerifyingKey};
use p256::elliptic_curve::rand_core::OsRng;
use tonic::Request;

use proto_common::RedeemCodeRequest;
use relay_controller::gateway::GatewayService;
use relay_controller::grants::{apply_delivery, GrantsTable};
use relay_controller::state::GatewayState;
use relay_proto::relay_gateway_server::RelayGateway;
use shared::client_signature::{ClientRegistration, ClientSignatureVerifier};

const NAMESPACE: &str = "tenant";
const PHONE_CODE: &str = "kJ8f2QwXnR4tYv6b";

fn grants_table(rows: &[(&str, &str, &str, &str)]) -> GrantsTable {
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
            name: Some("grants".into()),
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

fn fresh_public_key() -> (VerifyingKey, Vec<u8>) {
    let vk = *SigningKey::random(&mut OsRng).verifying_key();
    let sec1 = vk.to_encoded_point(false).as_bytes().to_vec();
    (vk, sec1)
}

fn verifier() -> Arc<ClientSignatureVerifier> {
    Arc::new(ClientSignatureVerifier::new(Duration::from_secs(300)))
}

async fn service_with(
    table: GrantsTable,
    verifier: Arc<ClientSignatureVerifier>,
    kube_client: Option<kube::Client>,
) -> GatewayService {
    let state = Arc::new(GatewayState::new(verifier, kube_client, NAMESPACE.into()));
    *state.grants().write().await = table;
    GatewayService::new(state)
}

/// Captures every non-GET body and 404s the registered-keys GET, so the
/// redemption takes its create path.
fn recording_kube_client(written: Arc<Mutex<Vec<serde_json::Value>>>) -> kube::Client {
    let svc = tower::service_fn(move |req: http::Request<KubeBody>| {
        let written = written.clone();
        async move {
            let (parts, body) = req.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes();

            if parts.method != http::Method::GET {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    written.lock().unwrap().push(v);
                }
                return Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(201)
                        .header("content-type", "application/json")
                        .body(KubeBody::from(bytes.to_vec()))
                        .unwrap(),
                );
            }

            Ok::<_, std::convert::Infallible>(
                http::Response::builder()
                    .status(404)
                    .header("content-type", "application/json")
                    .body(KubeBody::from(
                        br#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":404,"reason":"NotFound"}"#
                            .to_vec(),
                    ))
                    .unwrap(),
            )
        }
    });
    kube::Client::new(svc, NAMESPACE)
}

/// Parks the FIRST non-GET write until released, so a second redemption of
/// the same code runs while the first sits mid-`register_key`. Counts every
/// non-GET write. Only the first is gated: in the fixed code the loser never
/// reaches Kubernetes, so gating every write would deadlock.
fn gated_kube_client(
    writes: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
) -> kube::Client {
    let svc = tower::service_fn(move |req: http::Request<KubeBody>| {
        let writes = writes.clone();
        let entered = entered.clone();
        let release = release.clone();
        async move {
            let (parts, body) = req.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes();

            if parts.method == http::Method::GET {
                return Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(404)
                        .header("content-type", "application/json")
                        .body(KubeBody::from(
                            br#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":404,"reason":"NotFound"}"#
                                .to_vec(),
                        ))
                        .unwrap(),
                );
            }

            if writes.fetch_add(1, Ordering::SeqCst) == 0 {
                entered.notify_one();
                release.notified().await;
            }

            Ok::<_, std::convert::Infallible>(
                http::Response::builder()
                    .status(201)
                    .header("content-type", "application/json")
                    .body(KubeBody::from(bytes.to_vec()))
                    .unwrap(),
            )
        }
    });
    kube::Client::new(svc, NAMESPACE)
}

/// The one-shot property under concurrency. It is the whole revoke-and-
/// re-invite story: if two devices can present the same code at once, both
/// enroll and the operator has silently handed out a second permanent seat
/// on the row.
///
/// A read-lock check followed by a `register_key` await and a
/// separate write-lock insert leaves a window the width of a Kubernetes
/// roundtrip. Every serial test passes across it. This test parks the first
/// caller inside that window and runs the second through it.
#[tokio::test]
async fn concurrent_redemptions_of_one_code_spend_the_row_once() {
    let writes = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let v = verifier();
    let service = Arc::new(
        service_with(
            grants_table(&[("caleb-phone", "app", PHONE_CODE, "family")]),
            v.clone(),
            Some(gated_kube_client(
                writes.clone(),
                entered.clone(),
                release.clone(),
            )),
        )
        .await,
    );

    let (vk_a, key_a) = fresh_public_key();
    let (_vk_b, key_b) = fresh_public_key();

    let svc_a = service.clone();
    let task_a = tokio::spawn(async move {
        svc_a
            .redeem_code(Request::new(RedeemCodeRequest {
                code: PHONE_CODE.into(),
                public_key: key_a,
            }))
            .await
    });

    // A is now parked inside register_key, past its own spent-row check.
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect(
            "redemption A never reached the kube write; code lookup regressed before register_key",
        );

    let result_b = service
        .redeem_code(Request::new(RedeemCodeRequest {
            code: PHONE_CODE.into(),
            public_key: key_b,
        }))
        .await;

    release.notify_one();
    let result_a = task_a.await.expect("redemption task must not panic");

    let winners = [result_a.is_ok(), result_b.is_ok()]
        .iter()
        .filter(|ok| **ok)
        .count();
    assert_eq!(winners, 1, "exactly one redemption may spend the row");

    let loser = match (&result_a, &result_b) {
        (Err(e), Ok(_)) | (Ok(_), Err(e)) => e,
        _ => unreachable!("winner count already asserted"),
    };
    assert_eq!(
        loser.code(),
        tonic::Code::PermissionDenied,
        "the losing redemption must be refused as a spent row"
    );

    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "only the winner may write the registered-keys Secret"
    );

    let registrations = v.registrations();
    let map = registrations.read().await;
    assert_eq!(map.len(), 1, "one row, one registration");
    let stored = map
        .get("caleb-phone")
        .expect("the winner must be registered under the row key");
    if result_a.is_ok() {
        assert_eq!(
            stored.verifying_key, vk_a,
            "the registered key must be the winner's"
        );
    } else {
        assert_ne!(
            stored.verifying_key, vk_a,
            "the registered key must be the winner's, not the refused caller's"
        );
    }
}

/// 404s the merge-PATCH so `register_key` takes its create branch, and records
/// the POSTed Secret body. Distinct from `recording_kube_client`, which 201s
/// every non-GET and so never leaves the patch path.
fn kube_client_without_secret(created: Arc<Mutex<Vec<serde_json::Value>>>) -> kube::Client {
    let svc = tower::service_fn(move |req: http::Request<KubeBody>| {
        let created = created.clone();
        async move {
            let (parts, body) = req.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes();

            if parts.method == http::Method::POST {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    created.lock().unwrap().push(v);
                }
                return Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(201)
                        .header("content-type", "application/json")
                        .body(KubeBody::from(bytes.to_vec()))
                        .unwrap(),
                );
            }

            Ok::<_, std::convert::Infallible>(
                http::Response::builder()
                    .status(404)
                    .header("content-type", "application/json")
                    .body(KubeBody::from(
                        br#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":404,"reason":"NotFound"}"#
                            .to_vec(),
                    ))
                    .unwrap(),
            )
        }
    });
    kube::Client::new(svc, NAMESPACE)
}

/// The first-ever redemption in a namespace finds no registered-keys Secret, so
/// the merge-patch 404s and the create branch must run. Without this the
/// bootstrap path is untested: every other test 201s the patch, so a handler
/// that mistook 404 for a hard error, or that created the Secret under the
/// wrong name or with an empty payload, would still look green while no
/// operator could ever enroll their first device.
#[tokio::test]
async fn first_registration_creates_the_secret_when_the_patch_finds_none() {
    let created: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let service = service_with(
        grants_table(&[("caleb-phone", "app", PHONE_CODE, "family")]),
        verifier(),
        Some(kube_client_without_secret(created.clone())),
    )
    .await;

    let (_vk, public_key) = fresh_public_key();
    let expected = base64::engine::general_purpose::STANDARD.encode(&public_key);

    service
        .redeem_code(Request::new(RedeemCodeRequest {
            code: PHONE_CODE.into(),
            public_key,
        }))
        .await
        .expect("first-ever redemption must create the Secret");

    let bodies = created.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1, "exactly one create");
    let body = &bodies[0];
    assert_eq!(
        body.pointer("/metadata/name").and_then(|v| v.as_str()),
        Some("relay-registered-keys"),
        "the created Secret must carry the name the store reads back"
    );
    assert_eq!(
        body.pointer("/stringData/caleb-phone")
            .and_then(|v| v.as_str()),
        Some(expected.as_str()),
        "the created Secret must carry the row's key, base64 of the raw SEC1 point"
    );
}

/// A code that matches no row is refused, and nothing is created: not a row,
/// not a registration, not a Secret entry.
///
/// Drop the grants lookup and an attacker who obtains any historical code
/// still enrolls. The `kube_client: None` construction is what proves the
/// rejection happens before the store is touched: a handler that reached the
/// store first would answer `FailedPrecondition` here.
#[tokio::test]
async fn a_code_matching_no_grant_row_is_refused_and_creates_nothing() {
    let v = verifier();
    let service = service_with(
        grants_table(&[("caleb-phone", "app", PHONE_CODE, "family")]),
        v.clone(),
        None,
    )
    .await;

    let (_vk, public_key) = fresh_public_key();
    let err = service
        .redeem_code(Request::new(RedeemCodeRequest {
            code: "not-a-code-anyone-wrote".into(),
            public_key,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        v.registrations().read().await.is_empty(),
        "a failed redemption must register nothing"
    );
}

/// The row is spent: one grant row carries at most one registered device key,
/// and a second presentation of the same code is refused.
///
/// Without the one-shot guard a leaked code becomes a permanent second seat on
/// the row, and "revoke and re-invite" stops working.
#[tokio::test]
async fn a_row_that_already_has_a_registered_key_refuses_a_second_redemption() {
    let v = verifier();
    let (first_vk, _) = fresh_public_key();
    v.registrations().write().await.insert(
        "caleb-phone".into(),
        ClientRegistration {
            verifying_key: first_vk,
            workspace: "family".into(),
        },
    );

    let service = service_with(
        grants_table(&[("caleb-phone", "app", PHONE_CODE, "family")]),
        v.clone(),
        None,
    )
    .await;

    let (_second_vk, public_key) = fresh_public_key();
    let err = service
        .redeem_code(Request::new(RedeemCodeRequest {
            code: PHONE_CODE.into(),
            public_key,
        }))
        .await
        .unwrap_err();

    assert_eq!(
        err.code(),
        tonic::Code::PermissionDenied,
        "a spent row must be refused before the key store is reached"
    );
    let registrations = v.registrations();
    let map = registrations.read().await;
    assert_eq!(
        map.get("caleb-phone").map(|r| r.verifying_key),
        Some(first_vk),
        "the first device's key must survive the second attempt untouched"
    );
}

/// A code that *does* match a fresh row gets through the authorization decision
/// and reaches the registered-key store, observed here as `FailedPrecondition`
/// because this service has no kube client.
///
/// This is the positive counterpart the two rejection tests need.
/// A handler that refused everything would pass both of them; only this one
/// says the matching path is live. Match on the row *key* instead of the row's
/// `identity` and this reds, which matters because the key is operator-chosen
/// prose while the identity is the secret.
#[tokio::test]
async fn a_code_matching_a_fresh_row_reaches_the_registered_key_store() {
    let service = service_with(
        grants_table(&[("caleb-phone", "app", PHONE_CODE, "family")]),
        verifier(),
        None,
    )
    .await;

    let (_vk, public_key) = fresh_public_key();
    let err = service
        .redeem_code(Request::new(RedeemCodeRequest {
            code: PHONE_CODE.into(),
            public_key,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

/// The presented public key is persisted against the row it redeemed, in the
/// encoding the verifier rebuild reads back (base64 of the raw SEC1 point), and
/// the redemption answers with the row key, which is also the signing `kid` the
/// client will present from now on.
///
/// Key the Secret entry by the row's *identity* (the code) instead
/// of the row key and the rebuild in `registered_key_store.rs` finds nothing;
/// store DER rather than SEC1 and `VerifyingKey::from_sec1_bytes` refuses it on
/// the next restart. Both faults are invisible until a pod roll, and both are
/// caught here.
#[tokio::test]
async fn redemption_persists_the_presented_key_against_its_row() {
    let written: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let service = service_with(
        grants_table(&[("caleb-phone", "app", PHONE_CODE, "family")]),
        verifier(),
        Some(recording_kube_client(written.clone())),
    )
    .await;

    let (_vk, public_key) = fresh_public_key();
    let expected = base64::engine::general_purpose::STANDARD.encode(&public_key);

    let resp = service
        .redeem_code(Request::new(RedeemCodeRequest {
            code: PHONE_CODE.into(),
            public_key,
        }))
        .await
        .expect("a fresh row's code must redeem")
        .into_inner();

    assert_eq!(
        resp.client_name, "caleb-phone",
        "the redemption answers with the row key, which is the signing kid"
    );

    let bodies = written.lock().unwrap().clone();
    assert!(
        !bodies.is_empty(),
        "redemption must persist the key to the relay-owned Secret"
    );
    let stored = bodies
        .iter()
        .find_map(|body| {
            body.pointer("/data/caleb-phone")
                .and_then(|v| v.as_str())
                .and_then(|wire| {
                    base64::engine::general_purpose::STANDARD
                        .decode(wire)
                        .ok()
                        .and_then(|raw| String::from_utf8(raw).ok())
                })
                .or_else(|| {
                    body.pointer("/stringData/caleb-phone")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
        })
        .expect("no write carried a `caleb-phone` entry");

    assert_eq!(
        stored, expected,
        "the stored value must be base64 of the raw SEC1 point"
    );
}
