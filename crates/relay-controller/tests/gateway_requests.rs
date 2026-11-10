//! Request validation and pass-through on the gateway, past authorization.
//!
//! Every request here is authenticated and authorized for real against the
//! mock apiserver in `common::apiserver`, so each test exercises the check it
//! names and not an earlier refusal. Forwarding tests dial the loopback mock
//! harness in `common::harness`; the rest are refused before any dial.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::mpsc;

use proto_common::{
    channel_stream_outbound, tool_result_frame, AwaitToolResultRequest, CallToolRequest,
    CancelToolRequest, ChannelStreamIngestRequest, ChannelStreamReceiveRequest,
    ClearContextRequest, ClientResponse, CompactContextRequest, GetConversationHistoryRequest,
    GetTurnStateRequest, TakeBackMessageRequest, ToolComplete, ToolResultFrame, TurnState,
    TurnStateEvent, UserMessage,
};
use relay_controller::gateway::GatewayService;
use relay_controller::state::{ChannelStreamOpener, GatewayState};
use relay_proto::relay_gateway_server::RelayGateway;

mod common;
use common::apiserver::{
    bearer, bearer_unclassed, first_ack, next_item, state_for, state_with_harness, IdentityRow,
    K8s, ALICE, ALICE_TOKEN, BOB, BOB_TOKEN,
};
use common::harness::{spawn_mock_harness, MockHarness, MOCK_CALL_ID};

const ALICE_ROW: IdentityRow<'static> = ("user-alice", ALICE, "alpha,beta", &[]);

/// Alice's token, allowed on alpha and beta.
fn alice_on_alpha_and_beta() -> Arc<K8s> {
    K8s::with_token(ALICE_TOKEN, ALICE)
        .allow("ag-alpha", Some(ALICE))
        .allow("ag-beta", Some(ALICE))
}

/// The gateway with alice bound to alpha and beta; no harness answers.
async fn service() -> (GatewayService, Arc<GatewayState>) {
    let state = state_for(&alice_on_alpha_and_beta(), &[ALICE_ROW]).await;
    (GatewayService::new(state.clone()), state)
}

/// The gateway with alice bound to alpha and beta, whose harness is `mock`.
async fn service_on(mock: MockHarness) -> (GatewayService, Arc<GatewayState>) {
    let addr = spawn_mock_harness(mock).await;
    let state = state_with_harness(&alice_on_alpha_and_beta(), &[ALICE_ROW], addr).await;
    (GatewayService::new(state.clone()), state)
}

/// Mint a person channel stream for alice on alpha, as ChannelStreamReceive would.
async fn alice_channel_stream(state: &GatewayState) -> String {
    let (tx, rx) = mpsc::channel(4);
    // Keep the receiver alive so the channel stream stays deliverable.
    std::mem::forget(rx);
    state
        .mint_channel_stream(
            ChannelStreamOpener::Person,
            ALICE.into(),
            "alpha".into(),
            tx,
        )
        .await
}

fn turn_state() -> GetTurnStateRequest {
    GetTurnStateRequest {
        agent: "alpha".into(),
    }
}

fn client_response(
    channel_stream_id: &str,
    response: ClientResponse,
) -> ChannelStreamIngestRequest {
    ChannelStreamIngestRequest {
        channel_stream_id: channel_stream_id.into(),
        client_response: Some(response),
        ..Default::default()
    }
}

fn result_response(result_json: &str) -> ClientResponse {
    ClientResponse {
        request_id: "req-unknown".into(),
        result_json: result_json.into(),
        error: None,
    }
}

// --- GetTurnState -------------------------------------------------------------

#[tokio::test]
async fn get_turn_state_returns_idle_for_an_agent_with_no_recorded_turn() {
    let (svc, _) = service().await;
    let resp = svc
        .get_turn_state(bearer(ALICE_TOKEN, turn_state()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.state, TurnState::Idle as i32);
    // No phantom failure text.
    assert!(resp.reason.is_empty());
    assert!(resp.code.is_empty());
}

#[tokio::test]
async fn get_turn_state_reflects_recorded_failed_with_reason() {
    let (svc, state) = service().await;
    state
        .set_and_broadcast_turn_state(
            "alpha",
            TurnStateEvent {
                state: TurnState::Failed as i32,
                reason: "boom".into(),
                code: "13".into(),
                ..Default::default()
            },
        )
        .await;
    let resp = svc
        .get_turn_state(bearer(ALICE_TOKEN, turn_state()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.state, TurnState::Failed as i32);
    assert_eq!(resp.reason, "boom");
    assert_eq!(resp.code, "13");
}

/// Only the harness sets turn state: alice's accepted message leaves the
/// agent IDLE, and bob's open stream gets no turn-state frame from it. The
/// turn state is the agent's: once the harness delivers WORKING for alice's
/// message, bob, who reaches the agent, polls it and sees her turn as
/// WORKING.
#[tokio::test]
async fn ingesting_a_message_changes_no_turn_state_and_the_turn_the_harness_sets_is_the_agents() {
    let state = alice_and_bob_on_alpha().await;
    let svc = GatewayService::new(state.clone());
    let mut bob = open_alpha_stream(&svc, BOB_TOKEN).await;
    // The stream's replay of the idle agent.
    next_turn_state(&mut bob).await;
    let id = alice_channel_stream(&state).await;
    svc.channel_stream_ingest(bearer(
        ALICE_TOKEN,
        ChannelStreamIngestRequest {
            channel_stream_id: id,
            user_message: Some(UserMessage::default()),
            ..Default::default()
        },
    ))
    .await
    .unwrap();

    assert_eq!(
        bob_polls_alpha(&svc).await,
        TurnState::Idle as i32,
        "an accepted message leaves the agent IDLE"
    );
    while let Ok(Some(frame)) = tokio::time::timeout(Duration::from_millis(200), bob.next()).await {
        let frame = frame.expect("bob's stream stays open");
        assert!(
            !matches!(
                frame.command,
                Some(channel_stream_outbound::Command::TurnState(_))
            ),
            "an accepted message sends no turn-state frame, got {frame:?}"
        );
    }

    harness_delivers(&state, TurnState::Working).await;
    assert_eq!(next_turn_state(&mut bob).await, TurnState::Working);
    assert_eq!(bob_polls_alpha(&svc).await, TurnState::Working as i32);
}

/// The turn state bob reads when he polls alpha.
async fn bob_polls_alpha(svc: &GatewayService) -> i32 {
    svc.get_turn_state(bearer(BOB_TOKEN, turn_state()))
        .await
        .unwrap()
        .into_inner()
        .state
}

/// Alice and bob, both reaching alpha, with no harness answering.
async fn alice_and_bob_on_alpha() -> Arc<GatewayState> {
    state_for(&alice_and_bob_reach_alpha(), &ALICE_AND_BOB_ROWS).await
}

const ALICE_AND_BOB_ROWS: [IdentityRow<'static>; 2] = [
    ("user-alice", ALICE, "alpha", &[]),
    ("user-bob", BOB, "alpha", &[]),
];

fn alice_and_bob_reach_alpha() -> Arc<K8s> {
    K8s::with_tokens(&[(ALICE_TOKEN, ALICE), (BOB_TOKEN, BOB)])
        .allow("ag-alpha", Some(ALICE))
        .allow("ag-alpha", Some(BOB))
}

/// Open a person receive stream on alpha and take its ack.
async fn open_alpha_stream(
    svc: &GatewayService,
    token: &str,
) -> <GatewayService as RelayGateway>::ChannelStreamReceiveStream {
    let mut stream = svc
        .channel_stream_receive(bearer(
            token,
            ChannelStreamReceiveRequest {
                channel_hint: None,
                agent: "alpha".into(),
                sender: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    first_ack(&mut stream).await;
    stream
}

/// The state of the next turn-state frame on `stream`.
async fn next_turn_state(
    stream: &mut <GatewayService as RelayGateway>::ChannelStreamReceiveStream,
) -> TurnState {
    TurnState::try_from(next_turn_state_event(stream).await.state).unwrap()
}

async fn next_turn_state_event(
    stream: &mut <GatewayService as RelayGateway>::ChannelStreamReceiveStream,
) -> TurnStateEvent {
    loop {
        match next_item(stream).await {
            Some(Ok(frame)) => {
                if let Some(channel_stream_outbound::Command::TurnState(e)) = frame.command {
                    return e;
                }
            }
            other => panic!("expected a turn-state frame, got {other:?}"),
        }
    }
}

async fn harness_delivers(state: &Arc<GatewayState>, turn_state: TurnState) {
    harness_delivers_event(
        state,
        TurnStateEvent {
            state: turn_state as i32,
            ..Default::default()
        },
    )
    .await;
}

async fn harness_delivers_event(state: &Arc<GatewayState>, event: TurnStateEvent) {
    use relay_controller::internal::InternalService;
    use relay_proto::relay_internal_server::RelayInternal;

    let internal = InternalService::new(state.clone(), Some(Arc::new(FixedVerifier("alpha"))));
    let mut delivery = tonic::Request::new(relay_proto::DeliverTurnStateRequest {
        turn_state: Some(event),
    });
    delivery
        .metadata_mut()
        .insert("authorization", "Bearer harness-sa".parse().unwrap());
    internal.deliver_turn_state(delivery).await.unwrap();
}

/// Every person connected to the agent gets its turn start and end with the
/// harness's message_id, and a stream opened mid-turn starts on WORKING
/// rather than IDLE, with no message_id.
#[tokio::test]
async fn turn_state_reaches_every_stream_on_the_agent_and_a_stream_opened_mid_turn_replays_working()
{
    let state = alice_and_bob_on_alpha().await;
    let svc = GatewayService::new(state.clone());
    let mut alice = open_alpha_stream(&svc, ALICE_TOKEN).await;
    let mut bob = open_alpha_stream(&svc, BOB_TOKEN).await;
    // Each stream's replay of the idle agent.
    next_turn_state(&mut alice).await;
    next_turn_state(&mut bob).await;

    let working = TurnStateEvent {
        state: TurnState::Working as i32,
        message_id: "msg-1".into(),
        ..Default::default()
    };
    harness_delivers_event(&state, working.clone()).await;
    assert_eq!(next_turn_state_event(&mut alice).await, working);
    assert_eq!(next_turn_state_event(&mut bob).await, working);

    let mut late = open_alpha_stream(&svc, BOB_TOKEN).await;
    assert_eq!(
        next_turn_state_event(&mut late).await,
        TurnStateEvent {
            state: TurnState::Working as i32,
            ..Default::default()
        },
        "a stream opened mid-turn replays the agent's running turn, with no message_id"
    );

    harness_delivers(&state, TurnState::Idle).await;
    assert_eq!(next_turn_state(&mut alice).await, TurnState::Idle);
    assert_eq!(next_turn_state(&mut bob).await, TurnState::Idle);
    assert_eq!(next_turn_state(&mut late).await, TurnState::Idle);
}

/// Authenticates every internal caller as the harness of a fixed agent,
/// so `DeliverTurnState` reaches its handler without a kube client.
struct FixedVerifier(&'static str);

#[async_trait::async_trait]
impl shared::auth::TokenVerifier for FixedVerifier {
    async fn verify_token(&self, _token: &str) -> Result<String, tonic::Status> {
        Ok(self.0.to_string())
    }
}

/// The harness's terminal turn state replaces the WORKING it delivered at
/// turn start: the next poll of the agent sees the terminal state, not
/// WORKING.
#[tokio::test]
async fn get_turn_state_returns_the_terminal_state_the_harness_delivered() {
    let (svc, state) = service().await;
    harness_delivers(&state, TurnState::Working).await;
    harness_delivers(&state, TurnState::Cancelled).await;

    let resp = svc
        .get_turn_state(bearer(ALICE_TOKEN, turn_state()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.state, TurnState::Cancelled as i32);
}

// --- The listener class -------------------------------------------------------

/// A request that bypassed the per-listener layer carries no class and is
/// refused, valid bearer or not.
#[tokio::test]
async fn get_turn_state_rejects_missing_extension() {
    let (svc, _) = service().await;
    let err = svc
        .get_turn_state(bearer_unclassed(ALICE_TOKEN, turn_state()))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(err.message().contains("listener class"), "{err:?}");
}

// --- ChannelStreamIngest ------------------------------------------------------------

#[tokio::test]
async fn channel_stream_ingest_rejects_unregistered_channel_stream() {
    let (svc, _) = service().await;
    let err = svc
        .channel_stream_ingest(bearer(ALICE_TOKEN, ChannelStreamIngestRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "empty channel_stream_id"
    );
    assert!(
        err.message().contains("channel_stream_id required"),
        "{err:?}"
    );

    let err = svc
        .channel_stream_ingest(bearer(
            ALICE_TOKEN,
            ChannelStreamIngestRequest {
                channel_stream_id: "never-minted".into(),
                user_message: Some(UserMessage::default()),
                ..Default::default()
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
    assert!(
        err.message()
            .contains("channel_stream_id is not registered"),
        "{err:?}"
    );
}

#[tokio::test]
async fn channel_stream_ingest_rejects_both_payloads_or_an_empty_client_response() {
    let (svc, state) = service().await;
    let id = alice_channel_stream(&state).await;
    let err = svc
        .channel_stream_ingest(bearer(
            ALICE_TOKEN,
            ChannelStreamIngestRequest {
                channel_stream_id: id.clone(),
                user_message: Some(UserMessage::default()),
                client_response: Some(ClientResponse {
                    request_id: "r".into(),
                    result_json: "{}".into(),
                    error: None,
                }),
                ..Default::default()
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "both payloads set"
    );
    assert!(
        err.message()
            .contains("exactly one of user_message or client_response"),
        "{err:?}"
    );

    // A client_response carrying neither a result nor an error is malformed too.
    let err = svc
        .channel_stream_ingest(bearer(
            ALICE_TOKEN,
            client_response(&id, result_response("")),
        ))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "neither result nor error"
    );
    assert!(
        err.message()
            .contains("ClientResponse must carry result_json or error"),
        "{err:?}"
    );
}

/// A method advertised on an ingest becomes dispatchable to the channel stream;
/// otherwise send_server_notification refuses it as unsupported. A later
/// ingest with no supported_methods does not clobber it.
#[tokio::test]
async fn channel_stream_ingest_advertised_methods_persist_across_empty_ingests() {
    let (svc, state) = service().await;
    let id = alice_channel_stream(&state).await;
    let mut req = client_response(&id, result_response("{}"));
    req.supported_methods = vec!["RevealPath".into()];
    svc.channel_stream_ingest(bearer(ALICE_TOKEN, req))
        .await
        .unwrap();
    assert!(
        state
            .send_server_notification("alpha", "RevealPath", "{}".into())
            .await
            .is_ok(),
        "advertised method must be usable"
    );

    // A later ingest carrying no supported_methods keeps the set advertised before.
    svc.channel_stream_ingest(bearer(
        ALICE_TOKEN,
        client_response(&id, result_response("{}")),
    ))
    .await
    .unwrap();
    assert!(
        state
            .send_server_notification("alpha", "RevealPath", "{}".into())
            .await
            .is_ok(),
        "empty supported_methods must not clobber prior set"
    );
}

/// The relay stamps the sender itself: a sender the client writes is replaced
/// by the subject the caller authenticated as, both on the message handed to
/// the harness and on the take-back forwarded to it.
#[tokio::test]
async fn ingest_and_take_back_replace_a_client_sent_sender_with_the_callers_subject() {
    let seen_take_back: Arc<Mutex<Option<TakeBackMessageRequest>>> = Arc::default();
    let (svc, state) = service_on(MockHarness {
        seen_take_back: seen_take_back.clone(),
        ..Default::default()
    })
    .await;
    let mut harness_bus = state.subscribe_or_create("alpha").await;
    let id = alice_channel_stream(&state).await;
    svc.channel_stream_ingest(bearer(
        ALICE_TOKEN,
        ChannelStreamIngestRequest {
            channel_stream_id: id,
            user_message: Some(UserMessage {
                sender: BOB.into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    ))
    .await
    .unwrap();
    let queued = tokio::time::timeout(Duration::from_secs(5), harness_bus.recv())
        .await
        .expect("the message must reach the harness bus")
        .unwrap();
    assert_eq!(queued.sender, ALICE);

    svc.take_back_message(bearer(
        ALICE_TOKEN,
        TakeBackMessageRequest {
            agent: "alpha".into(),
            sender: BOB.into(),
        },
    ))
    .await
    .unwrap();
    let forwarded = seen_take_back.lock().unwrap().take();
    assert_eq!(forwarded.map(|r| r.sender), Some(ALICE.to_string()));
}

// --- ChannelStreamReceive -----------------------------------------------------------

#[tokio::test]
async fn channel_stream_receive_emits_ack_then_idle_replay() {
    let (svc, _) = service().await;
    let mut stream = svc
        .channel_stream_receive(bearer(
            ALICE_TOKEN,
            ChannelStreamReceiveRequest {
                channel_hint: Some("cli:test".into()),
                agent: "alpha".into(),
                sender: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let channel_stream_id = first_ack(&mut stream).await;
    assert!(!channel_stream_id.is_empty());
    let second = stream.next().await.unwrap().unwrap();
    match second.command {
        Some(channel_stream_outbound::Command::TurnState(e)) => {
            assert_eq!(e.state, TurnState::Idle as i32);
        }
        other => panic!("second frame must be a TurnState replay, got {other:?}"),
    }
}

// --- Conversation history ----------------------------------------------------

/// Bob, who reaches the agent, has his history request reach the harness
/// unchanged.
#[tokio::test]
async fn get_conversation_history_reaches_the_harness_unchanged_for_any_subject_on_the_agent() {
    let seen_history: Arc<Mutex<Option<GetConversationHistoryRequest>>> = Arc::default();
    let addr = spawn_mock_harness(MockHarness {
        seen_history: seen_history.clone(),
        ..Default::default()
    })
    .await;
    let state = state_with_harness(&alice_and_bob_reach_alpha(), &ALICE_AND_BOB_ROWS, addr).await;
    let svc = GatewayService::new(state);
    let sent = GetConversationHistoryRequest {
        limit: None,
        agent: "alpha".into(),
    };
    svc.get_conversation_history(bearer(BOB_TOKEN, sent.clone()))
        .await
        .unwrap();
    assert_eq!(*seen_history.lock().unwrap(), Some(sent));
}

#[tokio::test]
async fn clear_context_reaches_the_harness_unchanged() {
    let seen_clear: Arc<Mutex<Option<ClearContextRequest>>> = Arc::default();
    let (svc, _) = service_on(MockHarness {
        seen_clear: seen_clear.clone(),
        ..Default::default()
    })
    .await;
    let sent = ClearContextRequest {
        agent: "alpha".into(),
    };
    svc.clear_context(bearer(ALICE_TOKEN, sent.clone()))
        .await
        .unwrap();
    assert_eq!(*seen_clear.lock().unwrap(), Some(sent));
}

#[tokio::test]
async fn compact_context_reaches_the_harness_unchanged() {
    let seen_compact: Arc<Mutex<Option<CompactContextRequest>>> = Arc::default();
    let (svc, _) = service_on(MockHarness {
        seen_compact: seen_compact.clone(),
        ..Default::default()
    })
    .await;
    let sent = CompactContextRequest {
        agent: "alpha".into(),
    };
    svc.compact_context(bearer(ALICE_TOKEN, sent.clone()))
        .await
        .unwrap();
    assert_eq!(*seen_compact.lock().unwrap(), Some(sent));
}

// --- Tool forwarding ----------------------------------------------------------

/// The relay is a pipe for tool dispatch: the request reaches the harness
/// field-for-field and the harness's call_id comes back unaltered.
#[tokio::test]
async fn dispatch_tool_forwards_request_unmodified_and_returns_harness_call_id() {
    let seen_dispatch: Arc<Mutex<Option<CallToolRequest>>> = Arc::default();
    let (svc, _) = service_on(MockHarness {
        seen_dispatch: seen_dispatch.clone(),
        ..Default::default()
    })
    .await;
    let sent = CallToolRequest {
        name: "Bash".into(),
        input_json: r#"{"cmd":"ls"}"#.into(),
        agent: "alpha".into(),
    };
    let resp = svc
        .dispatch_tool(bearer(ALICE_TOKEN, sent.clone()))
        .await
        .unwrap();
    assert_eq!(resp.into_inner().call_id, MOCK_CALL_ID);
    assert_eq!(*seen_dispatch.lock().unwrap(), Some(sent));
}

/// Result frames are relayed 1:1, in order, with no filtering, truncation,
/// or reordering, and the await request itself reaches the harness untouched.
#[tokio::test]
async fn await_tool_result_streams_harness_frames_unmodified() {
    let frames = vec![
        ToolResultFrame {
            frame: Some(tool_result_frame::Frame::Stdout("out".into())),
        },
        ToolResultFrame {
            frame: Some(tool_result_frame::Frame::Complete(ToolComplete::default())),
        },
    ];
    let (frames_tx, frames_rx) = mpsc::channel(4);
    for frame in &frames {
        frames_tx.send(Ok(frame.clone())).await.unwrap();
    }
    drop(frames_tx);
    let seen_await: Arc<Mutex<Option<AwaitToolResultRequest>>> = Arc::default();
    let (svc, state) = service_on(MockHarness {
        result_frames: Mutex::new(Some(frames_rx)),
        seen_await: seen_await.clone(),
        ..Default::default()
    })
    .await;
    state
        .record_dispatched_call(MOCK_CALL_ID, ALICE, "alpha")
        .await;
    let sent = AwaitToolResultRequest {
        call_id: MOCK_CALL_ID.into(),
        agent: "alpha".into(),
    };
    let stream = svc
        .await_tool_result(bearer(ALICE_TOKEN, sent.clone()))
        .await
        .unwrap()
        .into_inner();
    let got: Vec<ToolResultFrame> = stream.map(|f| f.unwrap()).collect().await;
    assert_eq!(
        got, frames,
        "every harness frame reaches the caller in order and unaltered"
    );
    assert_eq!(*seen_await.lock().unwrap(), Some(sent));
}

/// Cancel forwards on the call_id alone and reports the harness's answer.
#[tokio::test]
async fn cancel_tool_forwards_call_id_and_returns_harness_cancelled() {
    let seen_cancel: Arc<Mutex<Option<CancelToolRequest>>> = Arc::default();
    let (svc, state) = service_on(MockHarness {
        seen_cancel: seen_cancel.clone(),
        ..Default::default()
    })
    .await;
    state
        .record_dispatched_call(MOCK_CALL_ID, ALICE, "alpha")
        .await;
    let sent = CancelToolRequest {
        call_id: MOCK_CALL_ID.into(),
    };
    let resp = svc
        .cancel_tool(bearer(ALICE_TOKEN, sent.clone()))
        .await
        .unwrap();
    assert!(resp.into_inner().cancelled);
    assert_eq!(*seen_cancel.lock().unwrap(), Some(sent));
}
