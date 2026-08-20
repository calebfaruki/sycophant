//! A delivery containing an invalid row produces a Warning Event on the
//! grants ConfigMap naming the row key and the reason.
//!
//! ConfigMaps have no status subresource, so the Event is the only surface an
//! operator gets. `kubectl describe configmap grants` is the whole user story,
//! and it shows `note`. That is why this test asserts the POSTed wire body
//! rather than a log line or a return value: a Warning Event that omits the row
//! key tells the operator something is wrong with a map of twenty rows and
//! nothing about which one.
//!
//! The contract this test pins:
//!
//! ```ignore
//! pub async fn publish_row_errors(
//!     client: &kube::Client,
//!     namespace: &str,
//!     grants: &ConfigMap,
//!     errors: &[RowError],
//! ) -> Result<(), String>;
//! ```
//!
//! The kube mock is the `tower::service_fn` pattern from
//! `crates/toolset-controller/tests/tool_job_image_from_spec.rs:56`.

use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::client::Body as KubeBody;

use relay_controller::grants::{apply_delivery, RowError};
use relay_controller::grants_watcher::publish_row_errors;

const NAMESPACE: &str = "tenant";

/// Captures the JSON body of every POST the code makes, and echoes each one
/// back as a 201 so `Api::create` deserializes a valid object.
fn recording_kube_client(posted: Arc<Mutex<Vec<serde_json::Value>>>) -> kube::Client {
    let svc = tower::service_fn(move |req: http::Request<KubeBody>| {
        let posted = posted.clone();
        async move {
            let (parts, body) = req.into_parts();
            let bytes = body
                .collect()
                .await
                .expect("mock kube: request body must collect")
                .to_bytes();

            if parts.method == http::Method::POST {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    posted.lock().unwrap().push(v);
                }
            }

            let resp = http::Response::builder()
                .status(201)
                .header("content-type", "application/json")
                .body(KubeBody::from(bytes.to_vec()))
                .expect("mock kube: build response");
            Ok::<_, std::convert::Infallible>(resp)
        }
    });
    kube::Client::new(svc, NAMESPACE)
}

fn grants_configmap(rows: &[(&str, &str)]) -> ConfigMap {
    let data = rows
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    ConfigMap {
        metadata: ObjectMeta {
            name: Some("grants".into()),
            namespace: Some(NAMESPACE.into()),
            uid: Some("11111111-2222-3333-4444-555555555555".into()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    }
}

fn note_of(event: &serde_json::Value) -> String {
    event
        .get("note")
        .and_then(|n| n.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Emit the Event with a generic note ("invalid grants row") and this reds on
/// the row-key assertion; emit it as `type: Normal` and it reds
/// on the type assertion, which matters because `kubectl describe` and every
/// alerting rule filter on Warning; skip publication entirely and it reds on the
/// count. Each of those three is a live way to leave the operator blind while
/// every parsing test in `grants_delivery.rs` stays green.
#[tokio::test]
async fn an_invalid_row_raises_a_warning_event_naming_the_row_and_the_reason() {
    let cm = grants_configmap(&[
        (
            "dad-telegram",
            "channel: carrier-pigeon\nidentity: x\nworkspace: family\n",
        ),
        (
            "caleb-phone",
            "channel: app\nidentity: kJ8f2QwXnR4tYv6b\nworkspace: family\n",
        ),
    ]);
    let (_table, errors) = apply_delivery(&cm);
    assert_eq!(errors.len(), 1, "fixture: exactly one row is invalid");

    let posted: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    publish_row_errors(
        &recording_kube_client(posted.clone()),
        NAMESPACE,
        &cm,
        &errors,
    )
    .await
    .expect("publishing a row error must not fail against a healthy apiserver");

    let events = posted.lock().unwrap().clone();
    assert_eq!(events.len(), 1, "one Warning Event per rejected row");
    let event = &events[0];

    assert_eq!(
        event.get("type").and_then(|t| t.as_str()),
        Some("Warning"),
        "a rejected authorization row is a Warning, not a Normal event"
    );
    assert_eq!(
        event.pointer("/regarding/kind").and_then(|k| k.as_str()),
        Some("ConfigMap"),
        "the Event must hang off the grants ConfigMap so `kubectl describe` finds it"
    );
    assert_eq!(
        event.pointer("/regarding/name").and_then(|n| n.as_str()),
        Some("grants")
    );

    let note = note_of(event);
    assert!(
        note.contains("dad-telegram"),
        "the note must name the offending row key; got: {note}"
    );
    assert!(
        note.contains("carrier-pigeon") || note.contains(&errors[0].reason),
        "the note must state why the row was rejected; got: {note}"
    );
    assert!(
        !note.contains("caleb-phone"),
        "the valid row must not be implicated; got: {note}"
    );
}

/// Three bad rows in one delivery produce three separately addressable
/// Warnings, not one aggregate.
///
/// Collapse the errors into a single Event ("3 grants rows are invalid") and
/// this reds on the count. An aggregate note is what makes the
/// second and third typo invisible once the operator fixes the first.
#[tokio::test]
async fn every_rejected_row_gets_its_own_warning_event() {
    let cm = grants_configmap(&[
        ("a", "channel: pigeon\nidentity: x\nworkspace: w\n"),
        ("b", "channel: app\nidentity: \nworkspace: w\n"),
        ("c", "channel: app\nidentity: x\nworkspace: \n"),
    ]);
    let (_table, errors) = apply_delivery(&cm);
    assert_eq!(errors.len(), 3, "fixture: all three rows are invalid");

    let posted: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    publish_row_errors(
        &recording_kube_client(posted.clone()),
        NAMESPACE,
        &cm,
        &errors,
    )
    .await
    .expect("publishing row errors must not fail");

    let events = posted.lock().unwrap().clone();
    assert_eq!(events.len(), 3);
    let notes: Vec<String> = events.iter().map(note_of).collect();
    for key in ["a", "b", "c"] {
        assert!(
            notes.iter().any(|n| n.contains(key)),
            "no Event named row {key}; notes were {notes:?}"
        );
    }
}

/// A clean delivery is silent. Publish unconditionally and every watch re-list
/// floods the namespace with Warnings, which trains the operator to ignore the
/// one surface that reports a bad row.
#[tokio::test]
async fn a_clean_delivery_publishes_nothing() {
    let cm = grants_configmap(&[(
        "caleb-phone",
        "channel: app\nidentity: kJ8f2QwXnR4tYv6b\nworkspace: family\n",
    )]);
    let errors: Vec<RowError> = Vec::new();

    let posted: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    publish_row_errors(
        &recording_kube_client(posted.clone()),
        NAMESPACE,
        &cm,
        &errors,
    )
    .await
    .expect("a clean delivery must not error");

    assert!(posted.lock().unwrap().is_empty());
}
