//! An in-process stand-in for the Kubernetes API, plus the request helpers
//! the gateway tests share.
//!
//! The kube API is stubbed with the `tower::service_fn` mock pattern: the mock
//! answers TokenReview from a fixed table and records every TokenReview /
//! SubjectAccessReview body it is handed, so the tests observe the exact spec
//! the relay submitted — verb, resource, name, namespace, audience — without
//! a cluster.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use http_body_util::BodyExt;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::client::Body as KubeBody;
use tonic::{Request, Response};

use relay_controller::listener::ListenerClass;
use relay_controller::state::GatewayState;
use shared::auth::CHANNEL_RELAY_AUDIENCE;

use super::{identity_cm, NAMESPACE};

pub(crate) const ALICE: &str = "https://issuer.example#alice";
pub(crate) const BOB: &str = "https://issuer.example#bob";
pub(crate) const CHANNEL_SLACK: &str = "system:serviceaccount:tenant-acme:ch-slack";
pub(crate) const ALICE_TOKEN: &str = "alice-person-token";
pub(crate) const BOB_TOKEN: &str = "bob-person-token";
pub(crate) const CHANNEL_TOKEN: &str = "slack-channel-sa-token";

/// One allowed-review decision: access reviews naming `name` (and, when
/// `user` is set, carrying that user) answer allowed; everything else answers
/// denied.
#[derive(Clone)]
pub(crate) struct Decision {
    name: String,
    user: Option<String>,
}

impl Decision {
    fn matches(&self, body: &serde_json::Value) -> bool {
        let attrs = body.pointer("/spec/resourceAttributes").unwrap_or(body);
        let name_ok = attrs.pointer("/name").and_then(|v| v.as_str()) == Some(self.name.as_str());
        let user_ok = match self.user.as_deref() {
            Some(u) => body.pointer("/spec/user").and_then(|v| v.as_str()) == Some(u),
            None => true,
        };
        name_ok && user_ok
    }
}

/// Whether the apiserver authenticates `username`'s token for the requested
/// `audiences`. A ServiceAccount token is bound to the channel audience, so
/// it authenticates only when the review asks for that audience; a person
/// token only when the review asks for none.
fn audience_accepts(username: &str, audiences: &[&str]) -> bool {
    if username.starts_with("system:serviceaccount:") {
        audiences.contains(&CHANNEL_RELAY_AUDIENCE)
    } else {
        audiences.is_empty()
    }
}

/// Fixed token→username table; records the Authorization header and body of
/// every request it is handed.
pub(crate) struct K8s {
    pub(crate) tokens: Mutex<HashMap<String, String>>,
    pub(crate) decisions: Mutex<Vec<Decision>>,
    seen: Mutex<Vec<(Option<String>, serde_json::Value)>>,
}

impl K8s {
    pub(crate) fn with_tokens(tokens: &[(&str, &str)]) -> Arc<Self> {
        Arc::new(Self {
            tokens: Mutex::new(
                tokens
                    .iter()
                    .map(|(t, s)| (t.to_string(), s.to_string()))
                    .collect(),
            ),
            decisions: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn with_token(token: &str, subject: &str) -> Arc<Self> {
        Self::with_tokens(&[(token, subject)])
    }

    /// Allow reviews naming `name`; when `user` is set, only reviews carried
    /// by that user.
    pub(crate) fn allow(self: &Arc<Self>, name: &str, user: Option<&str>) -> Arc<Self> {
        self.decisions.lock().unwrap().push(Decision {
            name: name.to_string(),
            user: user.map(str::to_string),
        });
        self.clone()
    }

    pub(crate) fn client(self: &Arc<Self>) -> kube::Client {
        let this = self.clone();
        let svc = tower::service_fn(move |req: http::Request<KubeBody>| {
            let this = this.clone();
            async move {
                let (parts, body) = req.into_parts();
                // The bare token, so tests can assert the caller's token is
                // never the credential an access review is submitted under.
                let bearer = parts
                    .headers
                    .get(http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .map(str::to_string);
                let bytes = body
                    .collect()
                    .await
                    .expect("mock kube: body must collect")
                    .to_bytes();
                let payload: serde_json::Value =
                    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);

                this.seen.lock().unwrap().push((bearer, payload.clone()));
                if parts.uri.path().contains("tokenreviews") {
                    let token = payload
                        .pointer("/spec/token")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default();
                    let requested = payload
                        .pointer("/spec/audiences")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let audiences: Vec<&str> = requested
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                        .unwrap_or_default();
                    let username = this
                        .tokens
                        .lock()
                        .unwrap()
                        .get(token)
                        .cloned()
                        .filter(|u| audience_accepts(u, &audiences));
                    let status = match username {
                        Some(u) => serde_json::json!({
                            "authenticated": true,
                            "audiences": requested,
                            "user": {"username": u},
                        }),
                        None => serde_json::json!({"authenticated": false}),
                    };
                    let mut resp = payload;
                    resp["status"] = status;
                    return Ok::<_, std::convert::Infallible>(json_201(&resp));
                }
                let allowed = this
                    .decisions
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|d| d.matches(&payload));
                let mut resp = payload;
                resp["status"] = serde_json::json!({"allowed": allowed});
                Ok::<_, std::convert::Infallible>(json_201(&resp))
            }
        });
        kube::Client::new(svc, NAMESPACE)
    }

    /// Every POST, in order: its bearer token and body — TokenReviews and
    /// access reviews alike.
    pub(crate) fn seen(&self) -> Vec<(Option<String>, serde_json::Value)> {
        self.seen.lock().unwrap().clone()
    }

    pub(crate) fn seen_access_reviews(&self) -> Vec<(Option<String>, serde_json::Value)> {
        self.seen()
            .into_iter()
            .filter(|(_, b)| b.pointer("/spec/resourceAttributes").is_some())
            .collect()
    }

    pub(crate) fn seen_token_reviews(&self) -> Vec<(Option<String>, serde_json::Value)> {
        self.seen()
            .into_iter()
            .filter(|(_, b)| b.pointer("/spec/token").is_some())
            .collect()
    }
}

/// The 201 Created answer the apiserver gives a review POST, echoing `resp`.
fn json_201(resp: &serde_json::Value) -> http::Response<KubeBody> {
    http::Response::builder()
        .status(201)
        .header("content-type", "application/json")
        .body(KubeBody::from(serde_json::to_vec(resp).unwrap()))
        .unwrap()
}

/// One relay-identity ConfigMap per subject, as the user chart renders them.
pub(crate) type IdentityRow<'a> = (&'a str, &'a str, &'a str, &'a [(&'a str, &'a str)]);

fn identity_cms(rows: &[IdentityRow]) -> Vec<ConfigMap> {
    rows.iter()
        .map(|(name, subject, agents, handles)| identity_cm(name, Some(subject), agents, handles))
        .collect()
}

/// Parse `rows` and swap them in as the live identity table, as the watcher
/// does on each delivery.
pub(crate) async fn load_identities(state: &GatewayState, rows: &[IdentityRow<'_>]) {
    let cms = identity_cms(rows);
    let (table, errors) = relay_controller::identity::apply_delivery(&cms);
    assert!(
        errors.is_empty(),
        "fixture identities must parse: {errors:?}"
    );
    *state.identity().write().await = table;
}

/// Gateway state on the mock apiserver with `rows` loaded.
pub(crate) async fn state_for(k8s: &Arc<K8s>, rows: &[IdentityRow<'_>]) -> Arc<GatewayState> {
    let state = Arc::new(GatewayState::new(Some(k8s.client()), NAMESPACE.into()));
    load_identities(&state, rows).await;
    state
}

/// Like [`state_for`], but every agent's harness is the one listening on
/// `harness`.
pub(crate) async fn state_with_harness(
    k8s: &Arc<K8s>,
    rows: &[IdentityRow<'_>],
    harness: std::net::SocketAddr,
) -> Arc<GatewayState> {
    let state = Arc::new(GatewayState::with_harness_template(
        Some(k8s.client()),
        NAMESPACE.into(),
        format!("http://{harness}"),
    ));
    load_identities(&state, rows).await;
    state
}

pub(crate) fn bearer<T>(token: &str, message: T) -> Request<T> {
    bearer_classed(token, ListenerClass::PersonToken, message)
}

/// The class the per-listener layer stamps in production: person requests
/// arrive on 9091, channel requests on 9092.
pub(crate) fn bearer_classed<T>(token: &str, class: ListenerClass, message: T) -> Request<T> {
    let mut req = bearer_unclassed(token, message);
    req.extensions_mut().insert(class);
    req
}

/// A request carrying a bearer token but no listener class, as if it had
/// bypassed the per-listener layer.
pub(crate) fn bearer_unclassed<T>(token: &str, message: T) -> Request<T> {
    let mut req = Request::new(message);
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    req
}

/// A streaming RPC's Ok variant is not `Debug`, so `unwrap_err` won't
/// compile. Extract the error directly.
pub(crate) fn stream_err<S>(result: Result<Response<S>, tonic::Status>) -> tonic::Status {
    match result {
        Ok(_) => panic!("expected an error from the streaming RPC"),
        Err(s) => s,
    }
}

/// The next item on an open stream, failing rather than hanging when none
/// arrives.
pub(crate) async fn next_item<S, T>(stream: &mut S) -> Option<Result<T, tonic::Status>>
where
    S: futures::Stream<Item = Result<T, tonic::Status>> + Unpin,
{
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("the next item must arrive")
}

/// The next item on a stream whose access was just revoked is
/// PermissionDenied, and the stream then ends.
pub(crate) async fn assert_cut_on_next_item<S, T>(stream: &mut S, why: &str)
where
    S: futures::Stream<Item = Result<T, tonic::Status>> + Unpin,
    T: std::fmt::Debug,
{
    match next_item(stream).await {
        Some(Err(status)) => assert_eq!(status.code(), tonic::Code::PermissionDenied),
        other => panic!("the next item after {why} must be PermissionDenied, got {other:?}"),
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("the stream must end, not hang")
            .is_none(),
        "the stream must end after the denial"
    );
}
