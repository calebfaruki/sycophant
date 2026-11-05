//! Every request is authorized against its named agent, per credential
//! class, through the Kubernetes authorization API. The relay holds no
//! authorization table of its own: a person token is a TokenReview-ed subject;
//! a channel token is a TokenReview-ed `ch-<channel>` ServiceAccount
//! whose sender resolves through the identity table. Either way the named
//! agent is checked by one SubjectAccessReview for the resolved subject,
//! under the relay's own credential.
//!
//! The kube API is the in-process mock in `common::apiserver`: it answers
//! TokenReview from a fixed table, binds each token to its audience the way
//! the apiserver does, and records every review body it is handed, so the
//! tests observe the exact spec the relay submitted — verb, resource, name,
//! namespace, audience — without a cluster.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::mpsc;
use tonic::Request;

use proto_common::{
    channel_stream_outbound, tool_result_frame, AwaitToolResultRequest, CallToolRequest,
    ChannelStreamIngestRequest, ChannelStreamReceiveRequest, ContentBlock, GetTurnStateRequest,
    ListAgentsRequest, ListGrantsRequest, ToolListUpdate, ToolResultFrame, TurnState, UserMessage,
    WatchToolsRequest,
};
use relay_controller::gateway::GatewayService;
use relay_controller::listener::ListenerClass;
use relay_proto::relay_gateway_server::RelayGateway;
use shared::auth::CHANNEL_RELAY_AUDIENCE;

mod common;
use common::apiserver::{
    assert_cut_on_next_item, bearer, bearer_classed, load_identities, next_item, state_for,
    state_with_harness, stream_err, K8s, ALICE, ALICE_TOKEN, BOB, BOB_TOKEN, CHANNEL_SLACK,
    CHANNEL_TOKEN,
};
use common::harness::{spawn_mock_harness, MockHarness};
use common::NAMESPACE;

async fn service(k8s: &Arc<K8s>) -> GatewayService {
    let state = state_for(
        k8s,
        &[
            (
                "user-alice",
                ALICE,
                "alpha,beta",
                &[("slack", "U0123ABCDEF")],
            ),
            ("user-bob", BOB, "gamma", &[]),
        ],
    )
    .await;
    GatewayService::new(state)
}

fn call_tool(agent: &str) -> CallToolRequest {
    CallToolRequest {
        name: "Bash".into(),
        input_json: "{}".into(),
        conversation_id: String::new(),
        agent: agent.into(),
    }
}

fn receive(agent: &str, sender: &str) -> ChannelStreamReceiveRequest {
    ChannelStreamReceiveRequest {
        channel_hint: None,
        agent: agent.into(),
        sender: sender.into(),
    }
}

// --- The person path -------------------------------------------------------

/// A person request names its agent; the relay checks it by a
/// SubjectAccessReview for the TokenReview-ed subject, in its own namespace,
/// under its own credential. An allowed review is observable as the call
/// being forwarded — here it reaches the harness dial and fails Unavailable,
/// the same signal the existing suite uses.
///
/// Reviewing a subject other than the authenticated one, or omitting the
/// relay's namespace, would stop the check tracking the caller's RBAC.
#[tokio::test]
async fn a_person_request_is_authorized_by_a_subject_review_for_the_caller() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE).allow("ag-alpha", Some(ALICE));
    let svc = service(&k8s).await;

    let err = tokio::time::timeout(
        Duration::from_secs(5),
        svc.dispatch_tool(bearer(ALICE_TOKEN, call_tool("alpha"))),
    )
    .await
    .expect("an allowed review must forward the call without hanging")
    .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unavailable);

    let reviews = k8s.seen_access_reviews();
    assert!(
        reviews.iter().any(|(_, r)| {
            r.pointer("/spec/resourceAttributes/name")
                .and_then(|v| v.as_str())
                == Some("ag-alpha")
                && r.pointer("/spec/user").and_then(|v| v.as_str()) == Some(ALICE)
                && r.pointer("/spec/resourceAttributes/namespace")
                    .and_then(|v| v.as_str())
                    == Some(NAMESPACE)
        }),
        "the relay must review ag-alpha for the authenticated subject in its \
         own namespace; got {reviews:?}"
    );
    assert!(
        reviews
            .iter()
            .all(|(token, _)| token.as_deref() != Some(ALICE_TOKEN)),
        "the caller's token is for its TokenReview only, never the credential \
         an access review rides; got {reviews:?}"
    );
}

/// The review answers not-allowed and the relay denies the request
/// before any harness dial.
#[tokio::test]
async fn a_person_request_the_review_denies_is_refused_without_a_harness_dial() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE);
    let svc = service(&k8s).await;

    let err = tokio::time::timeout(
        Duration::from_secs(1),
        svc.dispatch_tool(bearer(ALICE_TOKEN, call_tool("gamma"))),
    )
    .await
    .expect("a denied review must be refused before the harness is dialed")
    .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

/// No token at all is PermissionDenied before anything else runs. The
/// request carries its listener class, so the refusal is the missing bearer
/// and not the missing class.
#[tokio::test]
async fn a_person_request_without_a_bearer_token_is_refused() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE);
    let svc = service(&k8s).await;

    let mut req = Request::new(call_tool("alpha"));
    req.extensions_mut().insert(ListenerClass::PersonToken);
    let err = svc.dispatch_tool(req).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().contains("missing authorization metadata"),
        "the refusal must name the missing bearer; got {err:?}"
    );
    assert!(
        k8s.seen().is_empty(),
        "no review may run without a bearer; got {:?}",
        k8s.seen()
    );
}

/// A token the apiserver does not authenticate resolves to no subject.
#[tokio::test]
async fn a_token_the_apiserver_rejects_resolves_to_no_subject() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE);
    let svc = service(&k8s).await;

    let err = svc
        .list_agents(bearer("forged-token", ListAgentsRequest {}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

// --- ListAgents answers the watched table -------------------------------

/// ListAgents answers the subject's whole granted list from the watched
/// identity table. Authentication still costs one TokenReview; no access
/// review is submitted and nothing is enumerated.
///
/// Answering from Service enumeration (or any cluster-derived source) would
/// show an agent created after the grant, breaking the created-later
/// reach rule; authorizing by review would stop the list being a pure table
/// read.
#[tokio::test]
async fn list_agents_answers_the_watched_identity_table() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE);
    let svc = service(&k8s).await;

    let resp = svc
        .list_agents(bearer(ALICE_TOKEN, ListAgentsRequest {}))
        .await
        .expect("a bound subject must list its agents")
        .into_inner();
    let mut got = resp.agents;
    got.sort();
    assert_eq!(
        got,
        vec!["alpha".to_string(), "beta".to_string()],
        "the whole list from the identity ConfigMap, and nothing else"
    );
    assert!(
        k8s.seen_access_reviews().is_empty(),
        "ListAgents must not submit any access review or enumerate anything"
    );
    assert_eq!(
        k8s.seen_token_reviews().len(),
        1,
        "one TokenReview authenticates the caller"
    );
}

/// A subject with no identity ConfigMap gets an empty list, not an error and
/// not another subject's list.
#[tokio::test]
async fn list_agents_for_an_unbound_subject_is_empty() {
    let k8s = K8s::with_token("mallory-token", "https://issuer.example#mallory");
    let svc = service(&k8s).await;

    let resp = svc
        .list_agents(bearer("mallory-token", ListAgentsRequest {}))
        .await
        .expect("an empty list is a valid answer")
        .into_inner();
    assert!(resp.agents.is_empty());
}

// --- ListGrants authorizes, then answers ------------------------------------

/// ListGrants authorizes the named agent first; an unauthorized
/// agent yields PermissionDenied and no menu.
#[tokio::test]
async fn list_grants_for_an_unauthorized_agent_is_denied() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE);
    let svc = service(&k8s).await;

    let err = svc
        .list_grants(bearer(
            ALICE_TOKEN,
            ListGrantsRequest {
                agent: "gamma".into(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::PermissionDenied,
        "ListGrants must authorize the named agent before answering"
    );
}

/// A person token opens a receive stream on an agent it reaches: the
/// subject is the TokenReview-ed caller and the agent is reviewed for
/// it. This is how the app and terminal client start a chat turn.
///
/// Refusing person tokens here would leave no person-token client able to
/// start a model turn.
#[tokio::test]
async fn a_person_token_opens_a_receive_stream_on_a_reached_agent() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE).allow("ag-alpha", Some(ALICE));
    let svc = service(&k8s).await;

    let resp = svc
        .channel_stream_receive(bearer(ALICE_TOKEN, receive("alpha", "")))
        .await
        .expect("a person token on a reached agent must open a receive stream");
    drop(resp);

    let reviews = k8s.seen_access_reviews();
    assert!(
        reviews.iter().any(|(_, r)| {
            r.pointer("/spec/user").and_then(|v| v.as_str()) == Some(ALICE)
                && r.pointer("/spec/resourceAttributes/name")
                    .and_then(|v| v.as_str())
                    == Some("ag-alpha")
        }),
        "the receive must be reviewed for the authenticated subject; got {reviews:?}"
    );
}

/// A person token naming an agent it does not reach is refused.
#[tokio::test]
async fn a_person_receive_on_an_unreached_agent_is_denied() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE).allow("ag-alpha", Some(ALICE));
    let svc = service(&k8s).await;

    let err = stream_err(
        svc.channel_stream_receive(bearer(ALICE_TOKEN, receive("gamma", "")))
            .await,
    );
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

/// The sender field is the channel path's: a person token carrying one is
/// refused rather than resolved, so a person cannot name another subject's
/// handle.
#[tokio::test]
async fn a_person_receive_naming_a_sender_is_refused() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE).allow("ag-alpha", None);
    let svc = service(&k8s).await;

    let err = stream_err(
        svc.channel_stream_receive(bearer(ALICE_TOKEN, receive("alpha", "U0123ABCDEF")))
            .await,
    );
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

// --- Ingest: only the channel stream's opener may post on it ----------------------

/// A mock apiserver authenticating alice, bob, and the slack channel, with
/// both people allowed on ag-alpha and alice's slack handle bound.
fn alice_bob_and_channel() -> Arc<K8s> {
    K8s::with_tokens(&[
        (ALICE_TOKEN, ALICE),
        (BOB_TOKEN, BOB),
        (CHANNEL_TOKEN, CHANNEL_SLACK),
    ])
    .allow("ag-alpha", Some(ALICE))
    .allow("ag-alpha", Some(BOB))
}

/// The server-minted channel_stream_id from a receive stream's first frame, the
/// ChannelStreamAck.
async fn first_ack(
    stream: &mut <GatewayService as RelayGateway>::ChannelStreamReceiveStream,
) -> String {
    match stream.next().await {
        Some(Ok(frame)) => match frame.command {
            Some(channel_stream_outbound::Command::Ack(ack)) => ack.channel_stream_id,
            other => panic!("first frame must be the ChannelStreamAck, got {other:?}"),
        },
        other => panic!("receive stream yielded no first frame: {other:?}"),
    }
}

/// Open a receive stream and return its server-minted channel_stream_id from the
/// first frame.
async fn open_channel_stream(
    svc: &GatewayService,
    req: Request<ChannelStreamReceiveRequest>,
) -> String {
    let mut stream = svc
        .channel_stream_receive(req)
        .await
        .expect("the opener must be able to open its channel stream")
        .into_inner();
    let channel_stream_id = first_ack(&mut stream).await;
    // Keep the stream alive for the test's duration: dropping it unregisters
    // the channel stream after a drain delay.
    std::mem::forget(stream);
    channel_stream_id
}

fn ingest(channel_stream_id: &str) -> ChannelStreamIngestRequest {
    ChannelStreamIngestRequest {
        channel_stream_id: channel_stream_id.into(),
        user_message: Some(UserMessage {
            content: vec![ContentBlock::default()],
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Removing the RoleBinding cuts the next request: the reach review runs
/// fresh on every call and nothing caches an allow.
///
/// A cached first review would let the second dispatch reach the harness
/// (Unavailable here) after the binding is gone.
#[tokio::test]
async fn a_removed_rolebinding_denies_the_next_request() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE).allow("ag-alpha", Some(ALICE));
    let svc = service(&k8s).await;

    let err = svc
        .dispatch_tool(bearer(ALICE_TOKEN, call_tool("alpha")))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unavailable, "bound: {err:?}");

    k8s.decisions.lock().unwrap().clear();
    let err = svc
        .dispatch_tool(bearer(ALICE_TOKEN, call_tool("alpha")))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

/// Removing the RoleBinding cuts an open channel stream on its next message: the
/// reach review runs again on every ingest.
///
/// Without the per-ingest review the opener would keep posting to a
/// agent it no longer reaches.
#[tokio::test]
async fn a_removed_rolebinding_cuts_an_open_channel_streams_next_message() {
    let k8s = alice_bob_and_channel();
    let svc = service(&k8s).await;
    let ch = open_channel_stream(&svc, bearer(ALICE_TOKEN, receive("alpha", ""))).await;

    let err = svc
        .channel_stream_ingest(bearer(ALICE_TOKEN, ingest(&ch)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unavailable, "bound: {err:?}");

    k8s.decisions.lock().unwrap().clear();
    let err = svc
        .channel_stream_ingest(bearer(ALICE_TOKEN, ingest(&ch)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

/// Another person who reaches the same agent still cannot post on a
/// channel stream someone else opened.
#[tokio::test]
async fn another_persons_token_cannot_ingest_on_the_channel_stream() {
    let k8s = alice_bob_and_channel();
    let svc = service(&k8s).await;
    let ch = open_channel_stream(&svc, bearer(ALICE_TOKEN, receive("alpha", ""))).await;

    let err = svc
        .channel_stream_ingest(bearer(BOB_TOKEN, ingest(&ch)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

/// A channel cannot post on a channel stream a person opened, even for the same
/// subject's bound handle.
#[tokio::test]
async fn a_channel_cannot_ingest_on_a_person_channel_stream() {
    let k8s = alice_bob_and_channel();
    let svc = service(&k8s).await;
    let ch = open_channel_stream(&svc, bearer(ALICE_TOKEN, receive("alpha", ""))).await;

    let err = svc
        .channel_stream_ingest(bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            ingest(&ch),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

/// A person cannot post on a channel stream a channel opened for that person's
/// own handle: the channel-opened stream belongs to the channel.
#[tokio::test]
async fn a_person_cannot_ingest_on_a_channel_opened_stream() {
    let k8s = alice_bob_and_channel();
    let svc = service(&k8s).await;
    let ch = open_channel_stream(
        &svc,
        bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            receive("alpha", "U0123ABCDEF"),
        ),
    )
    .await;

    let err = svc
        .channel_stream_ingest(bearer(ALICE_TOKEN, ingest(&ch)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

// --- AwaitToolResult: only the dispatching subject reads a call ------------

/// The gateway with alice bound to alpha and beta, bob to alpha, and one
/// call alice dispatched on alpha.
async fn service_with_alices_call(k8s: &Arc<K8s>) -> GatewayService {
    let state = state_for(
        k8s,
        &[
            ("user-alice", ALICE, "alpha,beta", &[]),
            ("user-bob", BOB, "alpha", &[]),
        ],
    )
    .await;
    state
        .record_dispatched_call("alice-call", ALICE, "alpha")
        .await;
    GatewayService::new(state)
}

fn await_req(call_id: &str, agent: &str) -> AwaitToolResultRequest {
    AwaitToolResultRequest {
        call_id: call_id.into(),
        conversation_id: String::new(),
        agent: agent.into(),
    }
}

fn alice_and_bob_on_alpha() -> Arc<K8s> {
    K8s::with_tokens(&[(ALICE_TOKEN, ALICE), (BOB_TOKEN, BOB)])
        .allow("ag-alpha", Some(ALICE))
        .allow("ag-beta", Some(ALICE))
        .allow("ag-alpha", Some(BOB))
}

/// Another subject who reaches the same agent cannot read the call's
/// result by naming its call_id.
#[tokio::test]
async fn another_subject_cannot_await_a_call_it_did_not_dispatch() {
    let k8s = alice_and_bob_on_alpha();
    let svc = service_with_alices_call(&k8s).await;
    let err = stream_err(
        svc.await_tool_result(bearer(BOB_TOKEN, await_req("alice-call", "alpha")))
            .await,
    );
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

/// The call is read only on the agent it was dispatched to, even by its
/// owner.
#[tokio::test]
async fn a_call_is_not_awaited_through_another_agent() {
    let k8s = alice_and_bob_on_alpha();
    let svc = service_with_alices_call(&k8s).await;
    let err = stream_err(
        svc.await_tool_result(bearer(ALICE_TOKEN, await_req("alice-call", "beta")))
            .await,
    );
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

/// A call_id the relay never handed out is NotFound.
#[tokio::test]
async fn an_unknown_call_id_is_not_found() {
    let k8s = alice_and_bob_on_alpha();
    let svc = service_with_alices_call(&k8s).await;
    let err = stream_err(
        svc.await_tool_result(bearer(ALICE_TOKEN, await_req("no-such-call", "alpha")))
            .await,
    );
    assert_eq!(err.code(), tonic::Code::NotFound);
}

/// A channel-opened stream created while the sender's handle was bound stops
/// carrying that sender's messages as soon as the operator removes the
/// binding: every ingest re-resolves the handle against the live identity
/// table, the same way every request re-runs the reach review.
#[tokio::test]
async fn a_removed_handle_binding_cuts_an_open_channel_opened_stream() {
    let k8s = alice_bob_and_channel();
    let state = state_for(
        &k8s,
        &[("user-alice", ALICE, "alpha", &[("slack", "U0123ABCDEF")])],
    )
    .await;
    let svc = GatewayService::new(state.clone());

    let ch = open_channel_stream(
        &svc,
        bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            receive("alpha", "U0123ABCDEF"),
        ),
    )
    .await;

    // While bound, the channel's message passes the ingest check and reaches
    // the harness dial (Unavailable without a harness).
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        svc.channel_stream_ingest(bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            ingest(&ch),
        )),
    )
    .await
    .expect("a bound ingest must not hang")
    .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unavailable, "{err:?}");

    // The operator removes the handle; the watcher delivers the table
    // without it. The open channel stream's next message is refused.
    load_identities(&state, &[("user-alice", ALICE, "alpha", &[])]).await;

    let err = svc
        .channel_stream_ingest(bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            ingest(&ch),
        ))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::PermissionDenied,
        "a removed handle binding must cut the open channel stream: {err:?}"
    );
}

/// A stream opened before a revocation is cut on its next item: the relay
/// reviews the subject's reach again before each outbound frame, so removing
/// the RoleBinding ends an open receive stream the way it denies the next
/// request.
///
/// Without the per-item review the stream would keep delivering turn
/// traffic to a subject that no longer reaches the agent.
#[tokio::test]
async fn a_removed_rolebinding_cuts_an_open_receive_stream() {
    let k8s = alice_bob_and_channel();
    let state = state_for(&k8s, &[("user-alice", ALICE, "alpha", &[])]).await;
    let svc = GatewayService::new(state.clone());
    let mut stream = svc
        .channel_stream_receive(bearer(ALICE_TOKEN, receive("alpha", "")))
        .await
        .expect("alice reaches alpha")
        .into_inner();
    let channel_stream_id = first_ack(&mut stream).await;
    // The replayed turn phase still arrives while alice is bound.
    assert!(matches!(stream.next().await, Some(Ok(_))));

    // The operator removes alice's RoleBinding.
    k8s.decisions.lock().unwrap().clear();

    state
        .set_and_broadcast_turn_state(&channel_stream_id, ALICE, "conv-1", TurnState::Working)
        .await;
    assert_cut_on_next_item(&mut stream, "revocation").await;
}

/// An expired token cuts an open stream on its next item: the relay reviews
/// the stream's bearer token again before each outbound frame, so a session
/// that outlives its login stops, though the RoleBinding stays.
///
/// Without the per-item token review the stream would keep delivering
/// after the login expired.
#[tokio::test]
async fn an_expired_token_cuts_an_open_receive_stream() {
    let k8s = alice_bob_and_channel();
    let state = state_for(&k8s, &[("user-alice", ALICE, "alpha", &[])]).await;
    let svc = GatewayService::new(state.clone());
    let mut stream = svc
        .channel_stream_receive(bearer(ALICE_TOKEN, receive("alpha", "")))
        .await
        .expect("alice reaches alpha")
        .into_inner();
    let channel_stream_id = first_ack(&mut stream).await;
    assert!(matches!(stream.next().await, Some(Ok(_))));

    // The token expires: the apiserver no longer authenticates it.
    k8s.tokens.lock().unwrap().remove(ALICE_TOKEN);

    state
        .set_and_broadcast_turn_state(&channel_stream_id, ALICE, "conv-1", TurnState::Working)
        .await;
    assert_cut_on_next_item(&mut stream, "token expiry").await;
}

/// A channel's stream opened while the sender's handle was bound stops
/// delivering as soon as the operator removes the binding: before each
/// outbound item the relay re-resolves the handle, so the removed handle
/// receives none of the subject's turn traffic. The subject's RBAC reach is
/// untouched, so only the handle check can cut it.
///
/// Re-reviewing only the RBAC reach would keep this stream delivering to
/// the unbound handle.
#[tokio::test]
async fn a_removed_handle_binding_cuts_an_open_channel_receive_stream() {
    let k8s = alice_bob_and_channel();
    let state = state_for(
        &k8s,
        &[("user-alice", ALICE, "alpha", &[("slack", "U0123ABCDEF")])],
    )
    .await;
    let svc = GatewayService::new(state.clone());

    let mut stream = svc
        .channel_stream_receive(bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            receive("alpha", "U0123ABCDEF"),
        ))
        .await
        .expect("a bound sender opens its stream")
        .into_inner();
    let channel_stream_id = first_ack(&mut stream).await;
    assert!(
        matches!(stream.next().await, Some(Ok(_))),
        "replay arrives while bound"
    );

    // The operator removes alice's slack handle; her RoleBinding stays.
    load_identities(&state, &[("user-alice", ALICE, "alpha", &[])]).await;

    state
        .set_and_broadcast_turn_state(&channel_stream_id, ALICE, "conv-1", TurnState::Working)
        .await;
    assert_cut_on_next_item(&mut stream, "the handle removal").await;
}

// --- The channel path -------------------------------------------------------

/// A channel message names its agent and sender; the relay
/// authenticates the channel by a TokenReview pinned to the channel audience,
/// resolves the sender through the identity table, and checks the agent
/// by a SubjectAccessReview carrying the resolved subject and the relay's own
/// namespace.
#[tokio::test]
async fn a_channel_message_is_authorized_by_a_subject_review() {
    let k8s = K8s::with_token(CHANNEL_TOKEN, CHANNEL_SLACK).allow("ag-alpha", Some(ALICE));
    let svc = service(&k8s).await;

    let resp = svc
        .channel_stream_receive(bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            receive("alpha", "U0123ABCDEF"),
        ))
        .await
        .expect("a bound sender on an allowed agent must be received");
    drop(resp);

    // The TokenReview the channel path submitted is pinned to the channel
    // audience.
    let token_reviews = k8s.seen_token_reviews();
    assert_eq!(
        token_reviews.len(),
        1,
        "one TokenReview per channel request"
    );
    assert_eq!(
        token_reviews[0]
            .1
            .pointer("/spec/token")
            .and_then(|v| v.as_str()),
        Some(CHANNEL_TOKEN),
        "the TokenReview must carry the channel's token"
    );
    assert!(
        token_reviews[0]
            .1
            .pointer("/spec/audiences")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().any(|x| x.as_str() == Some(CHANNEL_RELAY_AUDIENCE)))
            .unwrap_or(false),
        "the channel TokenReview must pin the channel audience; got {}",
        token_reviews[0]
            .1
            .pointer("/spec/audiences")
            .cloned()
            .unwrap_or_default()
    );

    // The SubjectAccessReview carries the resolved subject, the agent's
    // harness Service, and the relay's own namespace.
    let reviews = k8s.seen_access_reviews();
    let sar = reviews
        .iter()
        .find(|(_, r)| r.pointer("/spec/user").and_then(|v| v.as_str()) == Some(ALICE))
        .expect("the relay must submit a SubjectAccessReview for the resolved subject");
    assert_eq!(
        sar.1
            .pointer("/spec/resourceAttributes/name")
            .and_then(|v| v.as_str()),
        Some("ag-alpha"),
        "the SAR must name the requested agent's harness Service"
    );
    assert_eq!(
        sar.1
            .pointer("/spec/resourceAttributes/namespace")
            .and_then(|v| v.as_str()),
        Some(NAMESPACE),
        "the SAR must carry RELAY_NAMESPACE, never a caller-supplied namespace"
    );
}

/// A sender whose platform ID has no operator binding is refused before
/// any review or routing.
#[tokio::test]
async fn a_channel_message_from_an_unbound_sender_is_refused() {
    let k8s = K8s::with_token(CHANNEL_TOKEN, CHANNEL_SLACK);
    let svc = service(&k8s).await;

    let err = stream_err(
        svc.channel_stream_receive(bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            receive("alpha", "U999ZZZZZZZ"),
        ))
        .await,
    );
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        k8s.seen_access_reviews().is_empty(),
        "an unbound sender must be refused before any access review"
    );
}

/// The channel path's structural channel identity: an authenticated token
/// that is not a `ch-<channel>` ServiceAccount of this tenant is
/// refused. The imposter is `ch-slack` from another namespace, whose
/// channel has a bound handle and whose subject the review allows, so only
/// the structural check stands between it and alice's stream.
#[tokio::test]
async fn a_non_channel_serviceaccount_is_refused_on_the_channel_path() {
    let k8s = K8s::with_token(CHANNEL_TOKEN, "system:serviceaccount:tenant-other:ch-slack")
        .allow("ag-alpha", Some(ALICE));
    let svc = service(&k8s).await;

    let err = stream_err(
        svc.channel_stream_receive(bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            receive("alpha", "U0123ABCDEF"),
        ))
        .await,
    );
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().contains("not a channel ServiceAccount"),
        "{}",
        err.message()
    );
}

/// The SAR answers not-allowed and the message is refused.
#[tokio::test]
async fn a_channel_message_for_an_unreachable_agent_is_refused() {
    let k8s = K8s::with_token(CHANNEL_TOKEN, CHANNEL_SLACK);
    let svc = service(&k8s).await;

    let err = stream_err(
        svc.channel_stream_receive(bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            receive("gamma", "U0123ABCDEF"),
        ))
        .await,
    );
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

// --- No review carries a caller-supplied namespace --------------------------

// --- Every path reaches the same list ---------------------------------------

// --- Revocation on the non-forwarding and tool-stream surfaces --------------

fn turn_state(agent: &str, conversation_id: &str) -> GetTurnStateRequest {
    GetTurnStateRequest {
        conversation_id: conversation_id.into(),
        agent: agent.into(),
    }
}

/// GetTurnState answers IDLE for any conversation it has not heard of, so a
/// poll from a subject whose RoleBinding was removed would get a well-formed
/// Ok without its own review. The reach review runs on every RPC on the
/// surface, not only the ones that forward.
///
/// Authorizing GetTurnState by the token alone would keep the revoked poll
/// answering Ok(IDLE).
#[tokio::test]
async fn get_turn_state_after_the_rolebinding_is_removed_is_refused() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE).allow("ag-alpha", Some(ALICE));
    let svc = service(&k8s).await;

    let idle = svc
        .get_turn_state(bearer(ALICE_TOKEN, turn_state("alpha", "conv-1")))
        .await
        .expect("a bound subject polls its turn state")
        .into_inner();
    assert_eq!(idle.state, TurnState::Idle as i32);

    k8s.decisions.lock().unwrap().clear();
    let err = svc
        .get_turn_state(bearer(ALICE_TOKEN, turn_state("alpha", "conv-1")))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::PermissionDenied,
        "a revoked subject must not read Ok(IDLE): {err:?}"
    );
}

/// Alice bound to alpha, with WatchTools and AwaitToolResult on alpha's
/// harness fed by the returned senders and one call she dispatched there.
async fn alice_on_a_streaming_harness(
    k8s: &Arc<K8s>,
) -> (
    GatewayService,
    mpsc::Sender<Result<ToolListUpdate, tonic::Status>>,
    mpsc::Sender<Result<ToolResultFrame, tonic::Status>>,
) {
    let (updates, updates_rx) = mpsc::channel(4);
    let (frames, frames_rx) = mpsc::channel(4);
    let addr = spawn_mock_harness(MockHarness {
        tool_updates: Mutex::new(Some(updates_rx)),
        result_frames: Mutex::new(Some(frames_rx)),
        ..Default::default()
    })
    .await;
    let state = state_with_harness(k8s, &[("user-alice", ALICE, "alpha", &[])], addr).await;
    state
        .record_dispatched_call("alice-call", ALICE, "alpha")
        .await;
    (GatewayService::new(state), updates, frames)
}

fn stdout_frame(text: &str) -> ToolResultFrame {
    ToolResultFrame {
        frame: Some(tool_result_frame::Frame::Stdout(text.into())),
    }
}

/// Open WatchTools as alice and see one update through while she is bound.
async fn open_watch_tools(
    svc: &GatewayService,
    updates: &mpsc::Sender<Result<ToolListUpdate, tonic::Status>>,
) -> <GatewayService as RelayGateway>::WatchToolsStream {
    let mut stream = svc
        .watch_tools(bearer(
            ALICE_TOKEN,
            WatchToolsRequest {
                agent: "alpha".into(),
            },
        ))
        .await
        .expect("alice reaches alpha")
        .into_inner();
    updates.send(Ok(ToolListUpdate::default())).await.unwrap();
    assert!(
        matches!(next_item(&mut stream).await, Some(Ok(_))),
        "an update arrives while bound"
    );
    stream
}

/// Open AwaitToolResult on alice's call and see one frame through while she
/// is bound.
async fn open_await_tool_result(
    svc: &GatewayService,
    frames: &mpsc::Sender<Result<ToolResultFrame, tonic::Status>>,
) -> <GatewayService as RelayGateway>::AwaitToolResultStream {
    let mut stream = svc
        .await_tool_result(bearer(ALICE_TOKEN, await_req("alice-call", "alpha")))
        .await
        .expect("alice awaits her own call")
        .into_inner();
    frames.send(Ok(stdout_frame("first"))).await.unwrap();
    assert!(
        matches!(next_item(&mut stream).await, Some(Ok(_))),
        "a frame arrives while bound"
    );
    stream
}

/// Removing the RoleBinding cuts an open WatchTools stream on its next
/// update, the way it denies the next request.
///
/// Passing the harness stream through without the per-item review would
/// keep the revoked subject receiving the agent's tool catalog.
#[tokio::test]
async fn a_removed_rolebinding_cuts_an_open_watch_tools_stream() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE).allow("ag-alpha", Some(ALICE));
    let (svc, updates, _frames) = alice_on_a_streaming_harness(&k8s).await;
    let mut stream = open_watch_tools(&svc, &updates).await;

    k8s.decisions.lock().unwrap().clear();
    updates.send(Ok(ToolListUpdate::default())).await.unwrap();
    assert_cut_on_next_item(&mut stream, "revocation").await;
}

/// Removing the RoleBinding cuts an open AwaitToolResult stream on its next
/// frame.
///
/// Passing the harness stream through without the per-item review would
/// keep the revoked subject reading the tool's output.
#[tokio::test]
async fn a_removed_rolebinding_cuts_an_open_await_tool_result_stream() {
    let k8s = K8s::with_token(ALICE_TOKEN, ALICE).allow("ag-alpha", Some(ALICE));
    let (svc, _updates, frames) = alice_on_a_streaming_harness(&k8s).await;
    let mut stream = open_await_tool_result(&svc, &frames).await;

    k8s.decisions.lock().unwrap().clear();
    frames.send(Ok(stdout_frame("second"))).await.unwrap();
    assert_cut_on_next_item(&mut stream, "revocation").await;
}

// --- A channel-opened stream's per-item token review ------------------------

/// Open a channel receive stream for alice's slack handle and read the ack
/// and the turn-state replay. Returns the stream and its channel_stream_id.
async fn open_channel_receive_stream(
    svc: &GatewayService,
) -> (
    <GatewayService as RelayGateway>::ChannelStreamReceiveStream,
    String,
) {
    let mut stream = svc
        .channel_stream_receive(bearer_classed(
            CHANNEL_TOKEN,
            ListenerClass::ChannelToken,
            receive("alpha", "U0123ABCDEF"),
        ))
        .await
        .expect("a bound sender opens its stream")
        .into_inner();
    let channel_stream_id = first_ack(&mut stream).await;
    assert!(
        matches!(next_item(&mut stream).await, Some(Ok(_))),
        "replay arrives while bound"
    );
    (stream, channel_stream_id)
}

/// The channel token is bound to the channel audience, so the re-review
/// before each item on a channel's stream must ask for that audience, as the
/// review that opened it did. The mock apiserver authenticates the channel
/// token for that audience only.
///
/// Re-reviewing the channel's token with no audience would make the
/// apiserver reject it, cutting the channel's own stream on its first
/// item.
#[tokio::test]
async fn a_channel_streams_per_item_reviews_carry_the_channel_audience() {
    let k8s = alice_bob_and_channel();
    let state = state_for(
        &k8s,
        &[("user-alice", ALICE, "alpha", &[("slack", "U0123ABCDEF")])],
    )
    .await;
    let svc = GatewayService::new(state.clone());
    let (mut stream, channel_stream_id) = open_channel_receive_stream(&svc).await;

    state
        .set_and_broadcast_turn_state(&channel_stream_id, ALICE, "conv-1", TurnState::Working)
        .await;
    assert!(
        matches!(next_item(&mut stream).await, Some(Ok(_))),
        "a live turn reaches the channel while it is bound"
    );

    let reviews = k8s.seen_token_reviews();
    // One to open, then one per item: the ack, the replay, the turn.
    assert_eq!(reviews.len(), 4, "{reviews:?}");
    for (_, review) in &reviews {
        let audiences = review
            .pointer("/spec/audiences")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            audiences,
            vec![serde_json::Value::from(CHANNEL_RELAY_AUDIENCE)],
            "every review of the channel's token must pin the channel audience"
        );
    }
}
