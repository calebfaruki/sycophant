use shared::auth::TokenVerifier;
use std::sync::Arc;
use tightbeam_controller::crd::ModelSpec;
use tightbeam_controller::grpc::ControllerService;
use tightbeam_controller::state::ControllerState;
use tightbeam_proto::tightbeam_controller_client::TightbeamControllerClient;
use tightbeam_proto::tightbeam_controller_server::TightbeamControllerServer;
use tightbeam_proto::{
    content_block, turn_event, turn_result_chunk, ContentBlock, ContentDelta, GetTurnRequest,
    RedeemEnrollmentRequest, StopReason, TextBlock, ToolCall, ToolUseInput, ToolUseStart,
    TurnComplete, TurnRequest, TurnResultChunk, TurnRole,
};
use tonic::transport::Server;

/// Test verifier that ignores the token and returns a fixed workspace name.
/// Lets integration tests bypass real auth without re-introducing a runtime
/// `"default"` fallback in production code.
struct FixedWorkspaceVerifier(String);

#[tonic::async_trait]
impl TokenVerifier for FixedWorkspaceVerifier {
    async fn verify_token(&self, _token: &str) -> Result<String, tonic::Status> {
        Ok(self.0.clone())
    }
}

use tightbeam_controller::conversation::test_support::{FailureModes, InjectableFactory};

/// Wrap a request body with a dummy `Authorization: Bearer test` header.
/// Required for any RPC that goes through `verify_workspace` (turn,
/// subscribe). The token contents are ignored by `FixedWorkspaceVerifier`.
fn bearer_authed<T>(inner: T) -> tonic::Request<T> {
    let mut req = tonic::Request::new(inner);
    req.metadata_mut()
        .insert("authorization", "Bearer test".parse().unwrap());
    req
}

async fn start_server() -> (String, Arc<ControllerState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");

    let tmp = tempfile::TempDir::new().unwrap();
    let log_dir = tmp.path().to_path_buf();
    let factory: Arc<dyn tightbeam_controller::conversation::ConversationStoreFactory> = Arc::new(
        tightbeam_controller::conversation::LocalFsFactory::new(log_dir),
    );
    let state = Arc::new(ControllerState::new(
        factory,
        None,
        "default".into(),
        "http://localhost:9090".into(),
        "ghcr.io/test/llm-job:latest".into(),
        shared::scheduling::SchedulingConfig::default(),
    ));
    state
        .set_model_spec(
            "default".into(),
            ModelSpec {
                provider_ref: tightbeam_controller::crd::ProviderRef {
                    name: "anthropic".into(),
                },
                model: "claude-sonnet-4-20250514".into(),
                params: None,
            },
        )
        .await;
    state
        .set_provider_spec(
            "anthropic".into(),
            tightbeam_controller::crd::ProviderSpec {
                format: "anthropic".into(),
                base_url: Some("https://api.anthropic.com/v1".into()),
                secret: tightbeam_controller::crd::ProviderSecret {
                    name: "anthropic-key".into(),
                    key: None,
                },
            },
        )
        .await;

    let pair = tightbeam_controller::grpc::InternalVerifierPair {
        transponder: Arc::new(FixedWorkspaceVerifier("default".to_string())),
        llm: Arc::new(FixedWorkspaceVerifier("default".to_string())),
    };
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let service = ControllerService::internal(state.clone(), Some(pair), signing_key);

    tokio::spawn(async move {
        let _tmp = tmp;
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        Server::builder()
            .layer(tightbeam_controller::audience_layer::RequiredAudienceLayer)
            .add_service(TightbeamControllerServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (url, state)
}

fn stream_turn_result_request(
    model: &str,
    chunks: Vec<TurnResultChunk>,
) -> tonic::Request<impl futures::Stream<Item = TurnResultChunk>> {
    let mut request = tonic::Request::new(futures::stream::iter(chunks));
    request
        .metadata_mut()
        .insert("x-tightbeam-model", model.parse().unwrap());
    // verify_workspace requires a bearer token; FixedWorkspaceVerifier
    // ignores the token value so any non-empty string suffices.
    request
        .metadata_mut()
        .insert("authorization", "Bearer test".parse().unwrap());
    request
}

/// Variant of `start_server` that injects an `InjectableFactory` armed
/// to fail on the chosen store method. Lets tests pin
/// `stream_turn_result`'s error-propagation: handler must return
/// `Status::internal`, not `Ok(TurnAck)`, when persistence fails.
async fn start_server_with_failing_store(modes: FailureModes) -> (String, Arc<ControllerState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");

    let factory: Arc<dyn tightbeam_controller::conversation::ConversationStoreFactory> =
        Arc::new(InjectableFactory(modes));
    let state = Arc::new(ControllerState::new(
        factory,
        None,
        "default".into(),
        "http://localhost:9090".into(),
        "ghcr.io/test/llm-job:latest".into(),
        shared::scheduling::SchedulingConfig::default(),
    ));
    state
        .set_model_spec(
            "default".into(),
            ModelSpec {
                provider_ref: tightbeam_controller::crd::ProviderRef {
                    name: "anthropic".into(),
                },
                model: "claude-sonnet-4-20250514".into(),
                params: None,
            },
        )
        .await;
    state
        .set_provider_spec(
            "anthropic".into(),
            tightbeam_controller::crd::ProviderSpec {
                format: "anthropic".into(),
                base_url: Some("https://api.anthropic.com/v1".into()),
                secret: tightbeam_controller::crd::ProviderSecret {
                    name: "anthropic-key".into(),
                    key: None,
                },
            },
        )
        .await;

    let pair = tightbeam_controller::grpc::InternalVerifierPair {
        transponder: Arc::new(FixedWorkspaceVerifier("default".to_string())),
        llm: Arc::new(FixedWorkspaceVerifier("default".to_string())),
    };
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let service = ControllerService::internal(state.clone(), Some(pair), signing_key);

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        Server::builder()
            .layer(tightbeam_controller::audience_layer::RequiredAudienceLayer)
            .add_service(TightbeamControllerServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (url, state)
}

/// Variant of `start_server` whose `InternalVerifierPair` has slot-tagged
/// verifiers — mainframe slot returns "mf-tag", llm slot returns
/// "llm-tag". Tests rely on the slot tag to prove which verifier ran for
/// a given gRPC method (i.e. that `pick_verifier`'s audience-routing
/// reaches the correct slot). Kills the `== → !=` mutant in
/// `grpc.rs::pick_verifier`.
async fn start_server_with_tagged_pair() -> (String, Arc<ControllerState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");

    let tmp = tempfile::TempDir::new().unwrap();
    let log_dir = tmp.path().to_path_buf();
    let factory: Arc<dyn tightbeam_controller::conversation::ConversationStoreFactory> = Arc::new(
        tightbeam_controller::conversation::LocalFsFactory::new(log_dir),
    );
    let state = Arc::new(ControllerState::new(
        factory,
        None,
        "default".into(),
        "http://localhost:9090".into(),
        "ghcr.io/test/llm-job:latest".into(),
        shared::scheduling::SchedulingConfig::default(),
    ));

    let pair = tightbeam_controller::grpc::InternalVerifierPair {
        transponder: Arc::new(FixedWorkspaceVerifier("mf-tag".to_string())),
        llm: Arc::new(FixedWorkspaceVerifier("llm-tag".to_string())),
    };
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let service = ControllerService::internal(state.clone(), Some(pair), signing_key);

    tokio::spawn(async move {
        let _tmp = tmp;
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        Server::builder()
            .layer(tightbeam_controller::audience_layer::RequiredAudienceLayer)
            .add_service(TightbeamControllerServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (url, state)
}

#[tokio::test]
async fn get_turn_uses_llm_slot_of_verifier_pair() {
    // Pin pick_verifier's audience-routing: GetTurn is an LLM method,
    // so the layer stamps RequiredAudience::Llm and pick_verifier MUST
    // select pair.llm. The tagged pair returns "llm-tag" from llm; if
    // pick_verifier wrongly picks mainframe (returns "mf-tag"), the
    // pending.workspace != caller_workspace check fires PermissionDenied.
    // The test asserts success — succeeds only when the llm slot was
    // used.
    let (url, state) = start_server_with_tagged_pair().await;
    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    // Enqueue a pending turn for workspace "llm-tag" so the GetTurn
    // workspace-match check passes only if the llm slot ran.
    state
        .set_model_spec(
            "default".into(),
            ModelSpec {
                provider_ref: tightbeam_controller::crd::ProviderRef {
                    name: "anthropic".into(),
                },
                model: "claude-sonnet-4-20250514".into(),
                params: None,
            },
        )
        .await;
    state
        .set_provider_spec(
            "anthropic".into(),
            tightbeam_controller::crd::ProviderSpec {
                format: "anthropic".into(),
                base_url: Some("https://api.anthropic.com/v1".into()),
                secret: tightbeam_controller::crd::ProviderSecret {
                    name: "anthropic-key".into(),
                    key: None,
                },
            },
        )
        .await;
    state.set_job_connected("default", true).await;
    let (result_tx, _result_rx) = tokio::sync::mpsc::channel::<TurnResultChunk>(16);
    state
        .enqueue_turn(
            "default",
            tightbeam_controller::state::PendingTurn {
                assignment: tightbeam_proto::TurnAssignment {
                    system: Some("test".into()),
                    tools: vec![],
                    messages: vec![],
                    params_json: None,
                },
                result_tx,
                workspace: "llm-tag".to_string(),
                conversation_id: "llm-tag.test-conv".into(),
                reply_channel: None,
                role: None,
                correlation_id: None,
                system_prompt: None,
            },
        )
        .await
        .expect("enqueue_turn must succeed");

    let resp = client
        .get_turn(bearer_authed(tightbeam_proto::GetTurnRequest {
            model_name: "default".into(),
        }))
        .await
        .expect("GetTurn must succeed — llm slot must be selected for GetTurn method");
    let _ = resp.into_inner();
}

#[tokio::test]
async fn get_turn_returns_unimplemented_when_no_pending() {
    let (url, _state) = start_server().await;
    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        client.get_turn(bearer_authed(GetTurnRequest {
            model_name: "default".into(),
        })),
    )
    .await;

    assert!(result.is_err(), "GetTurn should block when no turn pending");
}

/// Stream a single Complete chunk to `stream_turn_result` for the given
/// model. Helper shared by both failing-store tests so the bug-pin step
/// is the only thing that differs between the two test bodies.
async fn drive_stream_turn_result(
    state: &Arc<ControllerState>,
    url: &str,
    workspace: &str,
    conversation_id: &str,
) -> Result<tonic::Response<tightbeam_proto::TurnAck>, tonic::Status> {
    state.set_job_connected("default", true).await;
    let (result_tx, _result_rx) = tokio::sync::mpsc::channel::<TurnResultChunk>(16);
    state
        .enqueue_turn(
            "default",
            tightbeam_controller::state::PendingTurn {
                assignment: tightbeam_proto::TurnAssignment {
                    system: Some("test".into()),
                    tools: vec![],
                    messages: vec![],
                    params_json: None,
                },
                result_tx,
                workspace: workspace.to_string(),
                conversation_id: conversation_id.to_string(),
                reply_channel: None,
                role: None,
                correlation_id: None,
                system_prompt: None,
            },
        )
        .await
        .expect("enqueue_turn must succeed");

    let mut client = TightbeamControllerClient::connect(url.to_string())
        .await
        .unwrap();
    let _assignment = client
        .get_turn(bearer_authed(GetTurnRequest {
            model_name: "default".into(),
        }))
        .await
        .expect("get_turn must succeed");
    let chunks = vec![TurnResultChunk {
        chunk: Some(turn_result_chunk::Chunk::Complete(TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock {
                    text: "hello".into(),
                })),
            }],
            tool_calls: vec![],
        })),
    }];
    client
        .stream_turn_result(stream_turn_result_request("default", chunks))
        .await
}

#[tokio::test]
async fn stream_turn_result_propagates_conversation_load_error() {
    // Pin bug fix: if get_or_create_conversation Errs (read_all from the
    // store fails), handler MUST return Status::internal — not the
    // previously-buggy silent Ok(TurnAck). Audit blinding via log-store
    // DoS would otherwise be invisible to the LLM-job caller.
    let (url, state) = start_server_with_failing_store(FailureModes {
        read_all: true,
        ..FailureModes::default()
    })
    .await;
    let result = drive_stream_turn_result(&state, &url, "default", "default.test-conv").await;
    let err = result.expect_err("must Err when conversation load fails");
    assert_eq!(
        err.code(),
        tonic::Code::Internal,
        "expected Code::Internal on load failure, got {:?}: {}",
        err.code(),
        err.message(),
    );
}

#[tokio::test]
async fn stream_turn_result_propagates_append_error() {
    // Pin bug fix: if append_assistant_tagged Errs (write_event from the
    // store fails), handler MUST return Status::internal — not silently
    // discard the Err via the prior `let _ = ...`. Audit blinding via
    // log-store write failure would otherwise be invisible.
    let (url, state) = start_server_with_failing_store(FailureModes {
        write_event: true,
        ..FailureModes::default()
    })
    .await;
    let result = drive_stream_turn_result(&state, &url, "default", "default.test-conv").await;
    let err = result.expect_err("must Err when conversation append fails");
    assert_eq!(
        err.code(),
        tonic::Code::Internal,
        "expected Code::Internal on append failure, got {:?}: {}",
        err.code(),
        err.message(),
    );
}

#[tokio::test]
async fn end_to_end_turn_with_text_response() {
    let (url, state) = start_server().await;

    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();

        let assignment = client
            .get_turn(bearer_authed(GetTurnRequest {
                model_name: "default".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!assignment.messages.is_empty());
        let last_msg = assignment.messages.last().unwrap();
        assert_eq!(last_msg.role, "user");

        let chunks = vec![
            TurnResultChunk {
                chunk: Some(turn_result_chunk::Chunk::ContentDelta(ContentDelta {
                    text: "The answer ".into(),
                })),
            },
            TurnResultChunk {
                chunk: Some(turn_result_chunk::Chunk::ContentDelta(ContentDelta {
                    text: "is 42.".into(),
                })),
            },
            TurnResultChunk {
                chunk: Some(turn_result_chunk::Chunk::Complete(TurnComplete {
                    stop_reason: StopReason::EndTurn as i32,
                    content: vec![ContentBlock {
                        block: Some(content_block::Block::Text(TextBlock {
                            text: "The answer is 42.".into(),
                        })),
                    }],
                    tool_calls: vec![],
                })),
            },
        ];

        client
            .stream_turn_result(stream_turn_result_request("default", chunks))
            .await
            .unwrap();
    });

    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let mut response_stream = client
        .turn(bearer_authed(TurnRequest {
            system: Some("You are a test assistant.".into()),
            tools: vec![],
            messages: vec![tightbeam_proto::Message {
                role: "user".into(),
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock {
                        text: "What is the meaning of life?".into(),
                    })),
                }],
                tool_calls: vec![],
                tool_call_id: None,
                is_error: None,
            }],
            model: None,
            reply_channel: None,
            role: None,
            correlation_id: None,
            conversation_id: "default.test-conv".into(),
        }))
        .await
        .unwrap()
        .into_inner();

    let mut events = Vec::new();
    while let Some(event) = response_stream.message().await.unwrap() {
        events.push(event);
    }

    llm_job.await.unwrap();

    assert!(
        events.len() >= 2,
        "expected at least 2 events, got {}",
        events.len()
    );

    let has_delta = events
        .iter()
        .any(|e| matches!(e.event, Some(turn_event::Event::ContentDelta(_))));
    assert!(has_delta, "expected at least one ContentDelta");

    let has_complete = events
        .iter()
        .any(|e| matches!(e.event, Some(turn_event::Event::Complete(_))));
    assert!(has_complete, "expected a Complete event");

    let ws = state.get_or_create_workspace("default").await;
    let conv_arc = ws
        .get_or_create_conversation("default.test-conv")
        .await
        .unwrap();
    let conv = conv_arc.read().await;
    let history = conv.history();
    assert_eq!(history.len(), 2, "expected user + assistant messages");
    assert_eq!(history[0].role, "user");
    assert_eq!(history[1].role, "assistant");
    assert_eq!(
        tightbeam_providers::types::content_text(&history[1].content),
        Some("The answer is 42.")
    );
}

#[tokio::test]
async fn end_to_end_turn_with_tool_use() {
    let (url, state) = start_server().await;

    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();

        let _assignment = client
            .get_turn(bearer_authed(GetTurnRequest {
                model_name: "default".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        let chunks = vec![
            TurnResultChunk {
                chunk: Some(turn_result_chunk::Chunk::ToolUseStart(ToolUseStart {
                    id: "tc-1".into(),
                    name: "bash".into(),
                })),
            },
            TurnResultChunk {
                chunk: Some(turn_result_chunk::Chunk::ToolUseInput(ToolUseInput {
                    partial_json: r#"{"command":"ls"}"#.into(),
                })),
            },
            TurnResultChunk {
                chunk: Some(turn_result_chunk::Chunk::Complete(TurnComplete {
                    stop_reason: StopReason::ToolUse as i32,
                    content: vec![],
                    tool_calls: vec![ToolCall {
                        id: "tc-1".into(),
                        name: "bash".into(),
                        input_json: r#"{"command":"ls"}"#.into(),
                    }],
                })),
            },
        ];

        client
            .stream_turn_result(stream_turn_result_request("default", chunks))
            .await
            .unwrap();
    });

    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let mut response_stream = client
        .turn(bearer_authed(TurnRequest {
            system: None,
            tools: vec![],
            messages: vec![tightbeam_proto::Message {
                role: "user".into(),
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock {
                        text: "List files".into(),
                    })),
                }],
                tool_calls: vec![],
                tool_call_id: None,
                is_error: None,
            }],
            model: None,
            reply_channel: None,
            role: None,
            correlation_id: None,
            conversation_id: "default.test-conv".into(),
        }))
        .await
        .unwrap()
        .into_inner();

    let mut events = Vec::new();
    while let Some(event) = response_stream.message().await.unwrap() {
        events.push(event);
    }

    llm_job.await.unwrap();

    let has_tool_start = events
        .iter()
        .any(|e| matches!(e.event, Some(turn_event::Event::ToolUseStart(_))));
    assert!(has_tool_start, "expected ToolUseStart event");

    let complete = events.iter().find_map(|e| match &e.event {
        Some(turn_event::Event::Complete(c)) => Some(c),
        _ => None,
    });
    assert!(complete.is_some(), "expected Complete event");
    let complete = complete.unwrap();
    assert_eq!(complete.stop_reason, StopReason::ToolUse as i32);
    assert_eq!(complete.tool_calls.len(), 1);
    assert_eq!(complete.tool_calls[0].name, "bash");

    let ws = state.get_or_create_workspace("default").await;
    let conv_arc = ws
        .get_or_create_conversation("default.test-conv")
        .await
        .unwrap();
    let conv = conv_arc.read().await;
    let history = conv.history();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].role, "assistant");
    let tcs = history[1].tool_calls.as_ref().unwrap();
    assert_eq!(tcs[0].name, "bash");
}

#[tokio::test]
async fn assignment_carries_system_from_request() {
    let (url, _state) = start_server().await;

    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();

        let assignment = client
            .get_turn(bearer_authed(GetTurnRequest {
                model_name: "default".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(assignment.system, Some("Be helpful.".into()));

        let chunks = vec![TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Complete(TurnComplete {
                stop_reason: StopReason::EndTurn as i32,
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock {
                        text: "Done.".into(),
                    })),
                }],
                tool_calls: vec![],
            })),
        }];

        client
            .stream_turn_result(stream_turn_result_request("default", chunks))
            .await
            .unwrap();
    });

    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let mut stream = client
        .turn(bearer_authed(TurnRequest {
            system: Some("Be helpful.".into()),
            tools: vec![],
            messages: vec![tightbeam_proto::Message {
                role: "user".into(),
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock { text: "Hi".into() })),
                }],
                tool_calls: vec![],
                tool_call_id: None,
                is_error: None,
            }],
            model: None,
            reply_channel: None,
            role: None,
            correlation_id: None,
            conversation_id: "default.test-conv".into(),
        }))
        .await
        .unwrap()
        .into_inner();

    while stream.message().await.unwrap().is_some() {}
    llm_job.await.unwrap();
}

#[tokio::test]
async fn stream_turn_result_without_active_turn_fails() {
    let (url, _state) = start_server().await;
    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let chunks = vec![TurnResultChunk {
        chunk: Some(turn_result_chunk::Chunk::Complete(TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![],
            tool_calls: vec![],
        })),
    }];

    let status = client
        .stream_turn_result(stream_turn_result_request("default", chunks))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
}

#[tokio::test]
async fn stream_turn_result_denies_cross_workspace_caller() {
    // Closes audit criterion #3 (impersonate). Caller's token resolves to
    // "llm-tag" via the llm slot; the active turn is pre-loaded
    // for "other-ws". The handler MUST return NotFound (not
    // PermissionDenied — anti-leak per OWASP API1:2023 BOLA) and MUST
    // leave the slot intact so the legitimate workspace can still claim
    // the turn (strand-prevention).
    let (url, state) = start_server_with_tagged_pair().await;
    state
        .set_model_spec(
            "default".into(),
            ModelSpec {
                provider_ref: tightbeam_controller::crd::ProviderRef {
                    name: "anthropic".into(),
                },
                model: "claude-sonnet-4-20250514".into(),
                params: None,
            },
        )
        .await;
    let (result_tx, _result_rx) = tokio::sync::mpsc::channel::<TurnResultChunk>(16);
    state
        .set_active_turn(
            "default",
            "other-ws".into(),
            "other-ws.conv".into(),
            None,
            None,
            None,
            None,
            result_tx,
        )
        .await;

    let mut client = TightbeamControllerClient::connect(url).await.unwrap();
    let chunks = vec![TurnResultChunk {
        chunk: Some(turn_result_chunk::Chunk::Complete(TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![],
            tool_calls: vec![],
        })),
    }];

    let status = client
        .stream_turn_result(stream_turn_result_request("default", chunks))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);

    // Strand-prevention: the legitimate workspace's subsequent call must
    // still find the turn intact.
    let intact = state
        .take_active_turn_if_owned("default", "other-ws")
        .await
        .expect("legitimate owner should still find turn after wrong-workspace attempt");
    assert_eq!(intact.workspace, "other-ws");
}

#[tokio::test]
async fn turn_with_empty_messages_still_works() {
    let (url, state) = start_server().await;
    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();
        let _assignment = client
            .get_turn(bearer_authed(GetTurnRequest {
                model_name: "default".into(),
            }))
            .await
            .unwrap();

        let chunks = vec![TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Complete(TurnComplete {
                stop_reason: StopReason::EndTurn as i32,
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock { text: "ok".into() })),
                }],
                tool_calls: vec![],
            })),
        }];
        client
            .stream_turn_result(stream_turn_result_request("default", chunks))
            .await
            .unwrap();
    });

    let mut client = TightbeamControllerClient::connect(url).await.unwrap();
    let mut stream = client
        .turn(bearer_authed(TurnRequest {
            system: None,
            tools: vec![],
            messages: vec![],
            model: None,
            reply_channel: None,
            role: None,
            correlation_id: None,
            conversation_id: "default.test-conv".into(),
        }))
        .await
        .unwrap()
        .into_inner();

    while stream.message().await.unwrap().is_some() {}
    llm_job.await.unwrap();

    let ws = state.get_or_create_workspace("default").await;
    let conv_arc = ws
        .get_or_create_conversation("default.test-conv")
        .await
        .unwrap();
    let conv = conv_arc.read().await;
    assert_eq!(conv.history().len(), 1);
    assert_eq!(conv.history()[0].role, "assistant");
}

#[tokio::test]
async fn get_turn_before_turn_delivers() {
    let (url, _state) = start_server().await;

    let url_for_job = url.clone();
    let url_for_transponder = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = TightbeamControllerClient::connect(url_for_job)
            .await
            .unwrap();

        let assignment = client
            .get_turn(bearer_authed(GetTurnRequest {
                model_name: "default".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(assignment.messages.len(), 1);

        let chunks = vec![TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Complete(TurnComplete {
                stop_reason: StopReason::EndTurn as i32,
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock {
                        text: "done".into(),
                    })),
                }],
                tool_calls: vec![],
            })),
        }];
        client
            .stream_turn_result(stream_turn_result_request("default", chunks))
            .await
            .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let transponder = tokio::spawn(async move {
        let mut client = TightbeamControllerClient::connect(url_for_transponder)
            .await
            .unwrap();

        let mut stream = client
            .turn(bearer_authed(TurnRequest {
                system: None,
                tools: vec![],
                messages: vec![tightbeam_proto::Message {
                    role: "user".into(),
                    content: vec![ContentBlock {
                        block: Some(content_block::Block::Text(TextBlock {
                            text: "hello".into(),
                        })),
                    }],
                    tool_calls: vec![],
                    tool_call_id: None,
                    is_error: None,
                }],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
                conversation_id: "default.test-conv".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        let mut events = Vec::new();
        while let Some(event) = stream.message().await.unwrap() {
            events.push(event);
        }
        assert!(!events.is_empty(), "expected at least one event");
    });

    let timeout = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        futures::future::try_join(llm_job, transponder),
    )
    .await;

    match timeout {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => panic!("task panicked: {e}"),
        Err(_) => panic!("deadlock: GetTurn/Turn rendezvous timed out after 5s"),
    }
}

fn complete_chunk(text: &str) -> TurnResultChunk {
    TurnResultChunk {
        chunk: Some(turn_result_chunk::Chunk::Complete(TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
            }],
            tool_calls: vec![],
        })),
    }
}

fn user_text_message(text: &str) -> tightbeam_proto::Message {
    tightbeam_proto::Message {
        role: "user".into(),
        content: vec![ContentBlock {
            block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
        }],
        tool_calls: vec![],
        tool_call_id: None,
        is_error: None,
    }
}

#[tokio::test]
async fn delegate_turn_response_is_tagged_delegate() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (url, state) = start_server().await;

        let url_clone = url.clone();
        let llm_job = tokio::spawn(async move {
            let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();
            let _assignment = client
                .get_turn(bearer_authed(GetTurnRequest {
                    model_name: "default".into(),
                }))
                .await
                .unwrap()
                .into_inner();
            client
                .stream_turn_result(stream_turn_result_request(
                    "default",
                    vec![complete_chunk("delegate response")],
                ))
                .await
                .unwrap();
        });

        let mut client = TightbeamControllerClient::connect(url).await.unwrap();
        let mut stream = client
            .turn(bearer_authed(TurnRequest {
                system: Some("delegate prompt".into()),
                tools: vec![],
                messages: vec![user_text_message("delegate query")],
                model: None,
                reply_channel: None,
                role: Some(TurnRole::Delegate as i32),
                correlation_id: Some("call-xyz".into()),
                conversation_id: "default.test-conv".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        while stream.message().await.unwrap().is_some() {}
        llm_job.await.unwrap();

        let ws = state.get_or_create_workspace("default").await;
        let conv_arc = ws
            .get_or_create_conversation("default.test-conv")
            .await
            .unwrap();
        let conv = conv_arc.read().await;
        let raw = conv.history();

        assert_eq!(
            raw.len(),
            2,
            "raw history must include the user query and the delegate response"
        );
        assert_eq!(raw[0].role, "user");
        assert_eq!(raw[1].role, "assistant");

        let tags = conv.tags();
        assert_eq!(
            tags.first().and_then(|t| t.as_deref()),
            Some("delegate:call-xyz"),
            "delegate-role TurnRequest must tag the user query with delegate:<correlation_id>"
        );
        assert_eq!(
            tags.last().and_then(|t| t.as_deref()),
            Some("delegate:call-xyz"),
            "delegate-role TurnRequest must tag the assistant response with delegate:<correlation_id>"
        );

        let attr = conv.attributions();
        // User entry has no attribution.
        assert!(attr[0].model.is_none());
        assert!(attr[0].system_prompt_sha256.is_none());
        // Delegate assistant entry carries model and hash of the dispatched prompt.
        assert_eq!(attr[1].model.as_deref(), Some("default"));
        assert_eq!(
            attr[1].system_prompt_sha256.as_deref(),
            Some(
                tightbeam_controller::conversation::sha256_hex("delegate prompt").as_str()
            ),
            "system_prompt_sha256 must hash the prompt the LLM Job was given"
        );
    })
    .await
    .expect("test timed out");
}

/// Frontmatter on the system prompt routes the call to a model named in the
/// frontmatter's `model:` field. The body (post-strip) is what the LLM Job
/// receives; the audit hash on the log entry is computed on the pre-strip
/// value so external `sha256sum` matches a canonical persona file directly.
#[tokio::test]
async fn frontmatter_routes_to_named_model_and_strips_body() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (url, state) = start_server().await;

        // Register a second model named `smart` so frontmatter can route to it.
        state
            .set_model_spec(
                "smart".into(),
                ModelSpec {
                    provider_ref: tightbeam_controller::crd::ProviderRef {
                        name: "anthropic".into(),
                    },
                    model: "claude-sonnet-4-6".into(),
                    params: None,
                },
            )
            .await;

        let url_clone = url.clone();
        let llm_job = tokio::spawn(async move {
            let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();
            let assignment = client
                .get_turn(bearer_authed(GetTurnRequest {
                    model_name: "smart".into(),
                }))
                .await
                .unwrap()
                .into_inner();
            // The LLM Job must receive the post-strip body, not the frontmatter.
            assert_eq!(
                assignment.system.as_deref(),
                Some("You are Alice."),
                "LLM Job must receive frontmatter-stripped body"
            );
            client
                .stream_turn_result(stream_turn_result_request(
                    "smart",
                    vec![complete_chunk("hi")],
                ))
                .await
                .unwrap();
        });

        let raw = "---\nmodel: smart\n---\nYou are Alice.";

        let mut client = TightbeamControllerClient::connect(url).await.unwrap();
        let mut stream = client
            .turn(bearer_authed(TurnRequest {
                system: Some(raw.into()),
                tools: vec![],
                messages: vec![user_text_message("hi")],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
                conversation_id: "default.test-conv".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        while stream.message().await.unwrap().is_some() {}
        llm_job.await.unwrap();

        // The audit hash is computed on the pre-strip value so external
        // `sha256sum` of the canonical file matches directly.
        let ws = state.get_or_create_workspace("default").await;
        let conv_arc = ws
            .get_or_create_conversation("default.test-conv")
            .await
            .unwrap();
        let conv = conv_arc.read().await;
        let attrs = conv.attributions();
        let assistant_attrs: Vec<_> = conv
            .history()
            .iter()
            .zip(attrs.iter())
            .filter(|(m, _)| m.role == "assistant")
            .map(|(_, a)| a.clone())
            .collect();
        assert_eq!(assistant_attrs.len(), 1);
        assert_eq!(
            assistant_attrs[0].model.as_deref(),
            Some("smart"),
            "assistant entry should record the model resolved from frontmatter"
        );
        assert_eq!(
            assistant_attrs[0].system_prompt_sha256.as_deref(),
            Some(tightbeam_controller::conversation::sha256_hex(raw).as_str()),
            "audit hash must be computed on the pre-strip value"
        );
    })
    .await
    .expect("test timed out");
}

/// Regression: an orchestrator's continuation after a delegate call must run
/// under the orchestrator's own system prompt, not the delegate's. This
/// previously failed because the controller stored a workspace-level
/// system_prompt that the delegate's call overwrote and the orchestrator's
/// continuation (which sends `system: None` from the transponder is no longer
/// allowed) inherited.
#[tokio::test]
async fn orchestrator_continuation_uses_orchestrator_system_after_delegate() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (url, state) = start_server().await;

        // Drive three back-to-back turns through the LLM Job side. The job
        // accepts each assignment and immediately completes it.
        let url_clone = url.clone();
        let llm_job = tokio::spawn(async move {
            let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();
            for reply in &["orch one", "delegate reply", "orch wrap"] {
                let _assignment = client
                    .get_turn(bearer_authed(GetTurnRequest {
                        model_name: "default".into(),
                    }))
                    .await
                    .unwrap()
                    .into_inner();
                client
                    .stream_turn_result(stream_turn_result_request(
                        "default",
                        vec![complete_chunk(reply)],
                    ))
                    .await
                    .unwrap();
            }
        });

        let mut client = TightbeamControllerClient::connect(url).await.unwrap();

        // Turn 1: orchestrator user message.
        let mut s1 = client
            .turn(bearer_authed(TurnRequest {
                system: Some("ENTRYPOINT".into()),
                tools: vec![],
                messages: vec![user_text_message("hello")],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
                conversation_id: "default.test-conv".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        while s1.message().await.unwrap().is_some() {}

        // Turn 2: delegate call (different system).
        let mut s2 = client
            .turn(bearer_authed(TurnRequest {
                system: Some("DELEGATE_PROMPT".into()),
                tools: vec![],
                messages: vec![user_text_message("delegate query")],
                model: None,
                reply_channel: None,
                role: Some(TurnRole::Delegate as i32),
                correlation_id: Some("d1".into()),
                conversation_id: "default.test-conv".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        while s2.message().await.unwrap().is_some() {}

        // Turn 3: orchestrator continuation. Must carry ENTRYPOINT, not
        // DELEGATE_PROMPT — that's the regression we're guarding against.
        let mut s3 = client
            .turn(bearer_authed(TurnRequest {
                system: Some("ENTRYPOINT".into()),
                tools: vec![],
                messages: vec![tightbeam_proto::Message {
                    role: "tool".into(),
                    content: vec![],
                    tool_calls: vec![],
                    tool_call_id: Some("d1".into()),
                    is_error: None,
                }],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
                conversation_id: "default.test-conv".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        while s3.message().await.unwrap().is_some() {}

        llm_job.await.unwrap();

        let ws = state.get_or_create_workspace("default").await;
        let conv_arc = ws
            .get_or_create_conversation("default.test-conv")
            .await
            .unwrap();
        let conv = conv_arc.read().await;
        let attr = conv.attributions();

        let entrypoint_hash = tightbeam_controller::conversation::sha256_hex("ENTRYPOINT");
        let delegate_hash = tightbeam_controller::conversation::sha256_hex("DELEGATE_PROMPT");

        let assistant_attrs: Vec<_> = conv
            .history()
            .iter()
            .zip(attr.iter())
            .filter(|(m, _)| m.role == "assistant")
            .map(|(_, a)| a.clone())
            .collect();

        assert_eq!(
            assistant_attrs.len(),
            3,
            "expected three assistant entries: orchestrator-1, delegate, orchestrator-continuation"
        );
        assert_eq!(
            assistant_attrs[0].system_prompt_sha256.as_deref(),
            Some(entrypoint_hash.as_str()),
            "orchestrator turn 1 must hash ENTRYPOINT"
        );
        assert_eq!(
            assistant_attrs[1].system_prompt_sha256.as_deref(),
            Some(delegate_hash.as_str()),
            "delegate turn must hash DELEGATE_PROMPT"
        );
        assert_eq!(
            assistant_attrs[2].system_prompt_sha256.as_deref(),
            Some(entrypoint_hash.as_str()),
            "orchestrator continuation must hash ENTRYPOINT (not the delegate's prompt)"
        );
    })
    .await
    .expect("test timed out");
}

/// When a TurnRequest has neither frontmatter `model:` nor a non-empty
/// `params.model`, the runtime first checks for a registered model literally
/// named `default`; if present, it is used. The alphabetic-first fallback
/// only applies when no model named `default` is registered (covered by a
/// separate unit test in `state.rs`).
#[tokio::test]
async fn fallback_uses_reserved_default_when_present() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (url, state) = start_server().await;

        // start_server() registers `default`. Add `a-model` (alphabetic
        // first) — but reserved `default` should still win the fallback.
        state
            .set_model_spec(
                "a-model".into(),
                ModelSpec {
                    provider_ref: tightbeam_controller::crd::ProviderRef {
                        name: "anthropic".into(),
                    },
                    model: "claude-sonnet-4-20250514".into(),
                    params: None,
                },
            )
            .await;

        let url_clone = url.clone();
        let llm_job = tokio::spawn(async move {
            let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();
            let _assignment = client
                .get_turn(bearer_authed(GetTurnRequest {
                    model_name: "default".into(),
                }))
                .await
                .unwrap()
                .into_inner();
            client
                .stream_turn_result(stream_turn_result_request(
                    "default",
                    vec![complete_chunk("ok")],
                ))
                .await
                .unwrap();
        });

        let mut client = TightbeamControllerClient::connect(url).await.unwrap();
        let mut stream = client
            .turn(bearer_authed(TurnRequest {
                system: Some("plain prompt with no frontmatter".into()),
                tools: vec![],
                messages: vec![user_text_message("hi")],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
                conversation_id: "default.test-conv".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        while stream.message().await.unwrap().is_some() {}
        llm_job.await.unwrap();

        let ws = state.get_or_create_workspace("default").await;
        let conv_arc = ws
            .get_or_create_conversation("default.test-conv")
            .await
            .unwrap();
        let conv = conv_arc.read().await;
        let attrs: Vec<_> = conv
            .history()
            .iter()
            .zip(conv.attributions().iter())
            .filter(|(m, _)| m.role == "assistant")
            .map(|(_, a)| a.clone())
            .collect();
        assert_eq!(attrs.len(), 1);
        assert_eq!(
            attrs[0].model.as_deref(),
            Some("default"),
            "reserved `default` model name must win over alphabetic-first"
        );
    })
    .await
    .expect("test timed out");
}

/// `get_turn` rejects `GetTurnRequest` with an empty `model_name`. That call
/// shape used to silently fall back to `"default"`; now it errors.
#[tokio::test]
async fn get_turn_errors_when_model_name_empty() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (url, _state) = start_server().await;
        let mut client = TightbeamControllerClient::connect(url).await.unwrap();

        let status = client
            .get_turn(bearer_authed(GetTurnRequest {
                model_name: "".into(),
            }))
            .await
            .unwrap_err();

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(
            status.message().contains("model_name must be set"),
            "got: {:?}",
            status.message()
        );
    })
    .await
    .expect("test timed out");
}

/// With zero models registered, a TurnRequest that doesn't specify a model
/// (no frontmatter, no `params.model`) returns `failed_precondition` with
/// the named error.
#[tokio::test]
async fn errors_when_no_model_specified_and_registry_empty() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (url, state) = start_server().await;
        // start_server registers `default`; clear it.
        state.clear_models().await;

        let mut client = TightbeamControllerClient::connect(url).await.unwrap();
        let status = client
            .turn(bearer_authed(TurnRequest {
                system: Some("plain prompt".into()),
                tools: vec![],
                messages: vec![user_text_message("hi")],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
                conversation_id: "default.test-conv".into(),
            }))
            .await
            .unwrap_err();

        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert!(
            status.message().contains("no model specified"),
            "error must name the missing model: got {:?}",
            status.message()
        );
    })
    .await
    .expect("test timed out");
}

/// Returns the URL to dial and the controller's Ed25519 signing key so
/// the redeem_enrollment tests can mint enrollment codes the
/// controller will accept. The internal listener is wired without a
/// token verifier — these tests target the unauthenticated
/// `RedeemEnrollment` bypass RPC.
async fn start_server_with_signing_key() -> (String, ed25519_dalek::SigningKey) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");

    let tmp = tempfile::TempDir::new().unwrap();
    let log_dir = tmp.path().to_path_buf();
    let factory: Arc<dyn tightbeam_controller::conversation::ConversationStoreFactory> = Arc::new(
        tightbeam_controller::conversation::LocalFsFactory::new(log_dir),
    );
    let state = Arc::new(ControllerState::new(
        factory,
        None,
        "default".into(),
        "http://localhost:9090".into(),
        "ghcr.io/test/llm-job:latest".into(),
        shared::scheduling::SchedulingConfig::default(),
    ));

    let mut csprng = rand::rngs::OsRng;
    let signing_key = ed25519_dalek::SigningKey::generate(&mut csprng);
    let service = ControllerService::internal(state, None, signing_key.clone());

    tokio::spawn(async move {
        let _tmp = tmp;
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        Server::builder()
            .add_service(TightbeamControllerServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (url, signing_key)
}

/// Build a real SEC1-encoded P-256 public key for tests. Returns the
/// 65-byte uncompressed encoding the controller's SEC1 validation
/// accepts.
fn fresh_sec1_p256_public_key() -> Vec<u8> {
    let sk = p256::ecdsa::SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
    let vk = *sk.verifying_key();
    vk.to_encoded_point(false).as_bytes().to_vec()
}

#[tokio::test]
async fn redeem_enrollment_without_kube_client_returns_failed_precondition() {
    // The test harness wires the controller without a kube client.
    // After enrollment-code verification passes, redeem_for_client
    // can't reach the api.get path; the handler returns
    // FailedPrecondition. Proves the code-verify step is in the
    // handler (before the kube path).
    let (url, sk) = start_server_with_signing_key().await;
    let code_id = uuid::Uuid::new_v4().to_string();
    let enrollment_code =
        shared::auth::sign_enrollment_code(&sk, "hello-world", "calebs-iphone", &code_id, 3600);

    let mut client = TightbeamControllerClient::connect(url).await.unwrap();
    let err = client
        .redeem_enrollment(RedeemEnrollmentRequest {
            enrollment_code,
            public_key: fresh_sec1_p256_public_key(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

#[tokio::test]
async fn redeem_enrollment_rejects_code_signed_by_different_key() {
    let (url, _) = start_server_with_signing_key().await;
    let mut csprng = rand::rngs::OsRng;
    let other_sk = ed25519_dalek::SigningKey::generate(&mut csprng);
    let code = shared::auth::sign_enrollment_code(
        &other_sk,
        "hello-world",
        "calebs-iphone",
        "code-id",
        3600,
    );
    let mut client = TightbeamControllerClient::connect(url).await.unwrap();
    let err = client
        .redeem_enrollment(RedeemEnrollmentRequest {
            enrollment_code: code,
            public_key: fresh_sec1_p256_public_key(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

// ----------------------------------------------------------------------------
// Phase 2: internal-listener auth gap regression tests
// ----------------------------------------------------------------------------

#[tokio::test]
async fn get_turn_without_bearer_token_returns_permission_denied() {
    // Phase 2.1: GetTurn requires SA-token auth. The LLM Job uses
    // SaTokenInterceptor to attach Bearer <SA-token>; an in-cluster
    // pod that calls GetTurn without auth must be rejected so it can't
    // dequeue another workspace's pending turn.
    let (url, _state) = start_server().await;
    let mut client = TightbeamControllerClient::connect(url).await.unwrap();
    let err = client
        .get_turn(GetTurnRequest {
            model_name: "default".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().contains("missing authorization metadata")
            || err.message().contains("Bearer"),
        "expected bearer-token rejection, got: {:?}",
        err.message()
    );
}

#[tokio::test]
async fn channel_stream_rejects_workspace_mismatch_in_register() {
    // Phase 2.2: ChannelStream's ChannelRegister.workspace must match
    // the caller's auth-derived workspace. FixedWorkspaceVerifier
    // returns "default" for any token; a client that claims a
    // different workspace in ChannelRegister must be rejected.
    use tightbeam_proto::{channel_inbound, ChannelInbound, ChannelRegister};

    let (url, _state) = start_server().await;
    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let stream_items = vec![ChannelInbound {
        event: Some(channel_inbound::Event::Register(ChannelRegister {
            adapter_hint: Some("test:chan".into()),
            workspace: Some("not-default".into()), // mismatch with verifier's "default"
        })),
    }];
    let mut request = tonic::Request::new(futures::stream::iter(stream_items));
    request
        .metadata_mut()
        .insert("authorization", "Bearer test".parse().unwrap());
    let result = client.channel_stream(request).await;
    let err = match result {
        Err(e) => e,
        Ok(resp) => {
            // Stream may have opened; consume to surface the deferred error.
            let mut s = resp.into_inner();
            match futures::StreamExt::next(&mut s).await {
                Some(Err(e)) => e,
                other => panic!("expected workspace-mismatch error; got {:?}", other),
            }
        }
    };
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().contains("does not match"),
        "expected workspace-mismatch message, got: {:?}",
        err.message()
    );
}

// ----------------------------------------------------------------------------
// External-listener accessibility matrix tests
// ----------------------------------------------------------------------------
//
// These tests bind both listeners and assert which RPCs are reachable on the
// external (P-256 signed) listener. The audit found that `Turn` and
// `Subscribe` were on the external surface — a critical violation of the
// "transponder is the sole LLM-dispatch authority" thesis. Phase 1a closes
// that hole by moving them out of `signature_layer::ALLOWED_METHODS`.
//
// The tests sign every call with a registered P-256 keypair (matching what
// an enrolled Client CR would do). A signed Turn against the external
// listener must return `PermissionDenied` with message
// "method not allowed on external listener" — proves the classifier rejects
// the path BEFORE signature verification can succeed.

const TEST_EXT_WORKSPACE: &str = "hello-world";
const TEST_EXT_KID: &str = "client-alpha";

/// Bind both listeners on ephemeral ports and return everything a test needs
/// to make signed external calls + unsigned internal calls.
async fn start_server_with_external_listener() -> (
    String,                                                 // internal URL
    String,                                                 // external URL
    p256::ecdsa::SigningKey, // P-256 signing key for the registered client
    Arc<shared::client_signature::ClientSignatureVerifier>, // verifier (so tests can introspect if needed)
) {
    use shared::client_signature::{ClientRegistration, ClientSignatureVerifier};
    use shared::replay_cache::DEFAULT_WINDOW;
    use tightbeam_controller::signature_layer::SignatureLayer;

    let internal_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal_addr = internal_listener.local_addr().unwrap();
    let internal_url = format!("http://{internal_addr}");

    let external_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let external_addr = external_listener.local_addr().unwrap();
    let external_url = format!("http://{external_addr}");

    let tmp = tempfile::TempDir::new().unwrap();
    let log_dir = tmp.path().to_path_buf();
    let factory: Arc<dyn tightbeam_controller::conversation::ConversationStoreFactory> = Arc::new(
        tightbeam_controller::conversation::LocalFsFactory::new(log_dir),
    );
    let state = Arc::new(ControllerState::new(
        factory,
        None,
        "default".into(),
        "http://localhost:9090".into(),
        "ghcr.io/test/llm-job:latest".into(),
        shared::scheduling::SchedulingConfig::default(),
    ));
    state
        .set_model_spec(
            "default".into(),
            ModelSpec {
                provider_ref: tightbeam_controller::crd::ProviderRef {
                    name: "anthropic".into(),
                },
                model: "claude-sonnet-4-20250514".into(),
                params: None,
            },
        )
        .await;

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);

    // Register a P-256 client with the verifier. Tests sign requests with
    // this keypair; the middleware looks the kid up and verifies.
    let p256_sk = p256::ecdsa::SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
    let p256_vk = *p256_sk.verifying_key();
    let verifier = Arc::new(ClientSignatureVerifier::new(DEFAULT_WINDOW));
    verifier.registrations().write().await.insert(
        TEST_EXT_KID.to_string(),
        ClientRegistration {
            verifying_key: p256_vk,
            workspaces: vec![TEST_EXT_WORKSPACE.to_string()],
        },
    );

    let internal_pair = tightbeam_controller::grpc::InternalVerifierPair {
        transponder: Arc::new(FixedWorkspaceVerifier(TEST_EXT_WORKSPACE.to_string())),
        llm: Arc::new(FixedWorkspaceVerifier(TEST_EXT_WORKSPACE.to_string())),
    };
    let internal_service =
        ControllerService::internal(state.clone(), Some(internal_pair), signing_key.clone());
    let external_service =
        ControllerService::external(state.clone(), signing_key, verifier.clone());

    let verifier_for_layer = verifier.clone();
    tokio::spawn(async move {
        let _tmp = tmp;
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(internal_listener);
        Server::builder()
            .layer(tightbeam_controller::audience_layer::RequiredAudienceLayer)
            .add_service(TightbeamControllerServer::new(internal_service))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(external_listener);
        Server::builder()
            .layer(SignatureLayer::new(verifier_for_layer))
            .add_service(TightbeamControllerServer::new(external_service))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (internal_url, external_url, p256_sk, verifier)
}

/// Wrap protobuf bytes in the gRPC frame the controller's signature
/// middleware sees: `0x00 (no compression) || u32 BE length || payload`.
/// Matches the Flutter client's `frameGrpcMessage` in
/// `client/lib/src/signed_request.dart`.
fn frame_grpc_message(payload: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(5 + payload.len());
    framed.push(0x00);
    framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    framed.extend_from_slice(payload);
    framed
}

/// Build a tonic metadata map with valid signed-request headers for the
/// given (method, body) tuple. The body MUST be the raw protobuf bytes;
/// this helper frames them before hashing, matching what the signature
/// middleware sees when it collects the HTTP body.
fn sign_metadata(
    sk: &p256::ecdsa::SigningKey,
    method: &str,
    body_bytes: &[u8],
    kid: &str,
    workspace: &str,
) -> tonic::metadata::MetadataMap {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use p256::ecdsa::signature::Signer;
    use p256::ecdsa::Signature;
    use shared::client_signature::{
        body_hash_hex, signed_payload, SIG_BODY_HASH_HEADER, SIG_KID_HEADER, SIG_METHOD_HEADER,
        SIG_NONCE_HEADER, SIG_SIGNATURE_HEADER, SIG_TIMESTAMP_HEADER, SIG_WORKSPACE_HEADER,
    };

    let framed = frame_grpc_message(body_bytes);
    let body_hash = body_hash_hex(&framed);
    let nonce = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let payload = signed_payload(method, &body_hash, &nonce, ts);
    let sig: Signature = sk.sign(&payload);
    let sig_b64 = STANDARD.encode(sig.to_der().as_bytes());

    let mut md = tonic::metadata::MetadataMap::new();
    md.insert(SIG_METHOD_HEADER, method.parse().unwrap());
    md.insert(SIG_BODY_HASH_HEADER, body_hash.parse().unwrap());
    md.insert(SIG_NONCE_HEADER, nonce.parse().unwrap());
    md.insert(SIG_TIMESTAMP_HEADER, ts.to_string().parse().unwrap());
    md.insert(SIG_SIGNATURE_HEADER, sig_b64.parse().unwrap());
    md.insert(SIG_KID_HEADER, kid.parse().unwrap());
    md.insert(SIG_WORKSPACE_HEADER, workspace.parse().unwrap());
    md
}

/// Sibling of `sign_metadata` for RPCs that carry no workspace claim
/// (ListWorkspaces). Omits the `x-sig-workspace` header — the call IS
/// the authorization query.
fn sign_metadata_no_workspace(
    sk: &p256::ecdsa::SigningKey,
    method: &str,
    body_bytes: &[u8],
    kid: &str,
) -> tonic::metadata::MetadataMap {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use p256::ecdsa::signature::Signer;
    use p256::ecdsa::Signature;
    use shared::client_signature::{
        body_hash_hex, signed_payload, SIG_BODY_HASH_HEADER, SIG_KID_HEADER, SIG_METHOD_HEADER,
        SIG_NONCE_HEADER, SIG_SIGNATURE_HEADER, SIG_TIMESTAMP_HEADER,
    };

    let framed = frame_grpc_message(body_bytes);
    let body_hash = body_hash_hex(&framed);
    let nonce = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let payload = signed_payload(method, &body_hash, &nonce, ts);
    let sig: Signature = sk.sign(&payload);
    let sig_b64 = STANDARD.encode(sig.to_der().as_bytes());

    let mut md = tonic::metadata::MetadataMap::new();
    md.insert(SIG_METHOD_HEADER, method.parse().unwrap());
    md.insert(SIG_BODY_HASH_HEADER, body_hash.parse().unwrap());
    md.insert(SIG_NONCE_HEADER, nonce.parse().unwrap());
    md.insert(SIG_TIMESTAMP_HEADER, ts.to_string().parse().unwrap());
    md.insert(SIG_SIGNATURE_HEADER, sig_b64.parse().unwrap());
    md.insert(SIG_KID_HEADER, kid.parse().unwrap());
    md
}

#[tokio::test]
async fn external_listener_rejects_signed_turn() {
    // Phase 1a TDD-red→green. Before the ALLOWED_METHODS edit, this test
    // FAILS (the signed Turn reaches the handler and returns a different
    // error). After moving Turn out of ALLOWED_METHODS, the classifier
    // short-circuits the path with "method not allowed on external listener".
    //
    // Sends a Turn request over the external listener, signed with a
    // registered P-256 keypair. Asserts PermissionDenied + exact message.
    let (_internal_url, external_url, p256_sk, _verifier) =
        start_server_with_external_listener().await;

    let req = TurnRequest {
        system: None,
        tools: vec![],
        messages: vec![],
        model: None,
        reply_channel: None,
        role: None,
        correlation_id: None,
        conversation_id: "default.test-conv".into(),
    };

    use prost::Message as _;
    let body_bytes = req.encode_to_vec();
    let md = sign_metadata(
        &p256_sk,
        "/tightbeam.v1.TightbeamController/Turn",
        &body_bytes,
        TEST_EXT_KID,
        TEST_EXT_WORKSPACE,
    );

    let mut client = TightbeamControllerClient::connect(external_url)
        .await
        .unwrap();
    let mut request = tonic::Request::new(req);
    *request.metadata_mut() = md;
    let err = client.turn(request).await.unwrap_err();

    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message()
            .contains("method not allowed on external listener"),
        "expected classifier-Reject message, got: {:?}",
        err.message()
    );
}

#[tokio::test]
async fn external_listener_accepts_signed_channel_ingest() {
    // Signed two-step flow:
    //   1. ChannelReceive → server mints channel_id, returns it as the
    //      first ChannelAck frame on the outbound stream.
    //   2. ChannelIngest(channel_id) → handler accepts, stamps
    //      reply_channel = channel_id, routes via notify_subscriber.
    // We verify routing by opening a Subscribe stream on the INTERNAL
    // listener (FixedWorkspaceVerifier returns TEST_EXT_WORKSPACE) and
    // asserting the message arrives there with the minted channel_id
    // stamped as reply_channel.
    use tightbeam_proto::{
        channel_outbound, ChannelIngestRequest, ChannelReceiveRequest, SubscribeRequest,
        UserMessage,
    };

    let (internal_url, external_url, p256_sk, _verifier) =
        start_server_with_external_listener().await;

    // Open Subscribe on internal listener so we can observe the route.
    let mut internal_client = TightbeamControllerClient::connect(internal_url)
        .await
        .unwrap();
    let mut sub_stream = internal_client
        .subscribe(bearer_authed(SubscribeRequest {}))
        .await
        .unwrap()
        .into_inner();

    // Step 1: ChannelReceive → mint channel_id.
    let receive_req = ChannelReceiveRequest {
        adapter_hint: Some("flutter-app:test-chan".into()),
    };
    use prost::Message as _;
    let receive_body = receive_req.encode_to_vec();
    let receive_md = sign_metadata(
        &p256_sk,
        "/tightbeam.v1.TightbeamController/ChannelReceive",
        &receive_body,
        TEST_EXT_KID,
        TEST_EXT_WORKSPACE,
    );
    let mut external_client = TightbeamControllerClient::connect(external_url)
        .await
        .unwrap();
    let mut receive_request = tonic::Request::new(receive_req);
    *receive_request.metadata_mut() = receive_md;
    let mut receive_stream = external_client
        .channel_receive(receive_request)
        .await
        .unwrap()
        .into_inner();
    let first_frame = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        futures::StreamExt::next(&mut receive_stream),
    )
    .await
    .expect("first outbound frame within 500ms")
    .expect("stream not closed")
    .expect("no transport error");
    let channel_id = match first_frame.command {
        Some(channel_outbound::Command::Ack(ack)) => ack.channel_id,
        other => panic!("expected ChannelAck as first frame, got {other:?}"),
    };
    assert!(
        !channel_id.is_empty(),
        "minted channel_id must be non-empty"
    );

    // Step 2: ChannelIngest(channel_id).
    let ingest_req = ChannelIngestRequest {
        channel_id: channel_id.clone(),
        user_message: Some(UserMessage {
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock {
                    text: "hello agent".into(),
                })),
            }],
            sender: "tester".into(),
            reply_channel: None,
            conversation_id: String::new(),
        }),
        client_response: None,
        supported_methods: vec![],
        conversation_id: String::new(),
    };
    let ingest_body = ingest_req.encode_to_vec();
    let ingest_md = sign_metadata(
        &p256_sk,
        "/tightbeam.v1.TightbeamController/ChannelIngest",
        &ingest_body,
        TEST_EXT_KID,
        TEST_EXT_WORKSPACE,
    );
    let mut ingest_request = tonic::Request::new(ingest_req);
    *ingest_request.metadata_mut() = ingest_md;
    let ack = external_client
        .channel_ingest(ingest_request)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ack.channel_id, channel_id);

    // The ingested message must have flowed via notify_subscriber to the
    // workspace's Subscribe stream. reply_channel is stamped by the
    // ingest handler to the server-minted channel_id.
    let delivered = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        futures::StreamExt::next(&mut sub_stream),
    )
    .await
    .expect("subscribe stream produced a message within 500ms")
    .expect("stream not closed")
    .expect("no transport error");
    assert_eq!(delivered.sender, "tester");
    assert_eq!(
        delivered.reply_channel.as_deref(),
        Some(channel_id.as_str())
    );
}

#[tokio::test]
async fn external_listener_accepts_signed_mint_conversation() {
    // Canonical signed-and-allowed wiring test. Proves SignatureLayer
    // composes correctly, the signature verify pipeline (metadata
    // extraction → P-256 verify → body hash → public-key lookup)
    // works end-to-end, and OK status mapping is right.
    use tightbeam_proto::MintConversationRequest;

    let (_internal_url, external_url, p256_sk, _verifier) =
        start_server_with_external_listener().await;

    let req = MintConversationRequest {};
    use prost::Message as _;
    let body_bytes = req.encode_to_vec();
    let md = sign_metadata(
        &p256_sk,
        "/tightbeam.v1.TightbeamController/MintConversation",
        &body_bytes,
        TEST_EXT_KID,
        TEST_EXT_WORKSPACE,
    );

    let mut client = TightbeamControllerClient::connect(external_url)
        .await
        .unwrap();
    let mut request = tonic::Request::new(req);
    *request.metadata_mut() = md;
    let resp = client
        .mint_conversation(request)
        .await
        .unwrap()
        .into_inner();
    assert!(
        !resp.conversation_id.is_empty(),
        "mint should return a non-empty conversation_id"
    );
}

#[tokio::test]
async fn external_listener_rejects_channel_ingest_with_other_workspaces_channel_id() {
    // Load-bearing Phase 4 security test: workspace B cannot use
    // workspace A's server-minted channel_id to inject messages and
    // hijack A's reply path. The controller's channels map records
    // (channel_id → workspace) at mint time; ChannelIngest looks it up
    // and rejects with PermissionDenied when the caller's verified
    // workspace differs.
    use p256::ecdsa::SigningKey as P256SigningKey;
    use shared::client_signature::ClientRegistration;
    use tightbeam_proto::{
        channel_outbound, ChannelIngestRequest, ChannelReceiveRequest, UserMessage,
    };

    let (_internal_url, external_url, alpha_sk, verifier) =
        start_server_with_external_listener().await;

    // Register a SECOND client bound to a DIFFERENT workspace.
    let bravo_kid = "client-bravo";
    let bravo_workspace = "other-workspace";
    let bravo_sk = P256SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
    verifier.registrations().write().await.insert(
        bravo_kid.into(),
        ClientRegistration {
            verifying_key: *bravo_sk.verifying_key(),
            workspaces: vec![bravo_workspace.into()],
        },
    );

    // Step 1: alpha (hello-world) opens ChannelReceive and learns its channel_id.
    let mut alpha_client = TightbeamControllerClient::connect(external_url.clone())
        .await
        .unwrap();
    let receive_req = ChannelReceiveRequest {
        adapter_hint: Some("alpha".into()),
    };
    use prost::Message as _;
    let receive_body = receive_req.encode_to_vec();
    let receive_md = sign_metadata(
        &alpha_sk,
        "/tightbeam.v1.TightbeamController/ChannelReceive",
        &receive_body,
        TEST_EXT_KID,
        TEST_EXT_WORKSPACE,
    );
    let mut receive_request = tonic::Request::new(receive_req);
    *receive_request.metadata_mut() = receive_md;
    let mut receive_stream = alpha_client
        .channel_receive(receive_request)
        .await
        .unwrap()
        .into_inner();
    let first_frame = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        futures::StreamExt::next(&mut receive_stream),
    )
    .await
    .expect("first outbound frame within 500ms")
    .expect("stream not closed")
    .expect("no transport error");
    let alpha_channel_id = match first_frame.command {
        Some(channel_outbound::Command::Ack(ack)) => ack.channel_id,
        other => panic!("expected ChannelAck, got {other:?}"),
    };

    // Step 2: bravo (other-workspace) tries to send a ChannelIngest with alpha's channel_id.
    let mut bravo_client = TightbeamControllerClient::connect(external_url)
        .await
        .unwrap();
    let ingest_req = ChannelIngestRequest {
        channel_id: alpha_channel_id.clone(),
        user_message: Some(UserMessage {
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock {
                    text: "hijack attempt".into(),
                })),
            }],
            sender: "bravo".into(),
            reply_channel: None,
            conversation_id: String::new(),
        }),
        client_response: None,
        supported_methods: vec![],
        conversation_id: String::new(),
    };
    let ingest_body = ingest_req.encode_to_vec();
    let ingest_md = sign_metadata(
        &bravo_sk,
        "/tightbeam.v1.TightbeamController/ChannelIngest",
        &ingest_body,
        bravo_kid,
        bravo_workspace,
    );
    let mut ingest_request = tonic::Request::new(ingest_req);
    *ingest_request.metadata_mut() = ingest_md;
    let err = bravo_client
        .channel_ingest(ingest_request)
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().contains("different workspace"),
        "expected workspace-mismatch message, got: {:?}",
        err.message()
    );
}

#[tokio::test]
async fn external_listener_accepts_signed_get_conversation_history() {
    // Phase 4.4 wire test: GetConversationHistory is allowed on the
    // external listener so external clients can pull missed assistant
    // replies after a disconnect. The Phase 3.4 workspace-prefix check
    // on `conversation_id` still gates cross-workspace reads — verified
    // by `turn_rejects_conversation_id_from_other_workspace`.
    use prost::Message as _;
    use tightbeam_proto::{GetConversationHistoryRequest, MintConversationRequest};

    let (_internal_url, external_url, p256_sk, _verifier) =
        start_server_with_external_listener().await;

    let mut client = TightbeamControllerClient::connect(external_url)
        .await
        .unwrap();

    // Mint via a signed external call so the conversation_id lives in
    // the workspace's registry. GetConversationHistory now enforces
    // registry membership, not just the workspace prefix.
    let mint_req = MintConversationRequest {};
    let mint_body = mint_req.encode_to_vec();
    let mint_md = sign_metadata(
        &p256_sk,
        "/tightbeam.v1.TightbeamController/MintConversation",
        &mint_body,
        TEST_EXT_KID,
        TEST_EXT_WORKSPACE,
    );
    let mut mint_request = tonic::Request::new(mint_req);
    *mint_request.metadata_mut() = mint_md;
    let conversation_id = client
        .mint_conversation(mint_request)
        .await
        .unwrap()
        .into_inner()
        .conversation_id;
    assert!(
        conversation_id.starts_with(&format!("{TEST_EXT_WORKSPACE}.")),
        "minted id should carry the workspace prefix"
    );

    let req = GetConversationHistoryRequest {
        conversation_id: conversation_id.clone(),
        limit: None,
    };
    let body_bytes = req.encode_to_vec();
    let md = sign_metadata(
        &p256_sk,
        "/tightbeam.v1.TightbeamController/GetConversationHistory",
        &body_bytes,
        TEST_EXT_KID,
        TEST_EXT_WORKSPACE,
    );

    let mut request = tonic::Request::new(req);
    *request.metadata_mut() = md;
    let resp = client
        .get_conversation_history(request)
        .await
        .unwrap()
        .into_inner();
    assert!(
        resp.entries.is_empty(),
        "empty history expected for a freshly-minted conv_id, got {} entries",
        resp.entries.len()
    );
}

#[tokio::test]
async fn external_listener_rejects_unsigned_mint_conversation() {
    // Proves the classifier routes allowed methods through
    // VerifyAndForward (not Bypass). An unsigned call to an allowed
    // method must return Unauthenticated — if it returned OK, the
    // signature verify branch was skipped (bypass leak).
    use tightbeam_proto::MintConversationRequest;

    let (_internal_url, external_url, _p256_sk, _verifier) =
        start_server_with_external_listener().await;

    let mut client = TightbeamControllerClient::connect(external_url)
        .await
        .unwrap();
    let err = client
        .mint_conversation(tonic::Request::new(MintConversationRequest {}))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().contains("invalid signature"),
        "expected 'invalid signature' (sig verify branch), got: {:?}",
        err.message()
    );
}

#[tokio::test]
async fn turn_rejects_conversation_id_from_other_workspace() {
    // Phase 3.4: conversation_ids carry a `<workspace>.` prefix. A Turn
    // request authenticated as workspace "default" whose conversation_id
    // begins with another workspace's prefix must be rejected, even
    // though today's per-workspace store keying makes the property
    // already hold. Defends against future store-flattening refactors.
    let (url, _state) = start_server().await;
    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let request = bearer_authed(TurnRequest {
        system: None,
        tools: vec![],
        messages: vec![],
        model: None,
        reply_channel: None,
        role: None,
        correlation_id: None,
        conversation_id: "someone-else.test-conv".into(),
    });

    let err = client.turn(request).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().contains("workspace prefix"),
        "expected prefix-mismatch message, got: {:?}",
        err.message()
    );
}

#[tokio::test]
async fn external_listener_list_workspaces_returns_authorized_set() {
    // End-to-end smoke for the ListWorkspaces external RPC. Exercises:
    //   - Real signed-envelope middleware path WITHOUT the workspace
    //     header (the new VerifyNoWorkspace classification arm).
    //   - Real handler reading VerifiedClient extension and resolving
    //     the kid against the verifier's cache.
    //   - Real Client CR's spec.workspaces returned to the device.
    use shared::client_signature::ClientRegistration;
    use tightbeam_proto::ListWorkspacesRequest;

    let (_internal_url, external_url, p256_sk, verifier) =
        start_server_with_external_listener().await;

    // Replace the default single-workspace registration with a
    // multi-workspace one so the assertion can prove the full set is
    // returned, not just a happy-path scalar.
    let p256_vk = *p256_sk.verifying_key();
    verifier.registrations().write().await.insert(
        TEST_EXT_KID.to_string(),
        ClientRegistration {
            verifying_key: p256_vk,
            workspaces: vec!["ws-alpha".to_string(), "ws-beta".to_string()],
        },
    );

    let req = ListWorkspacesRequest {};
    use prost::Message as _;
    let body_bytes = req.encode_to_vec();
    let md = sign_metadata_no_workspace(
        &p256_sk,
        "/tightbeam.v1.TightbeamController/ListWorkspaces",
        &body_bytes,
        TEST_EXT_KID,
    );

    let mut client = TightbeamControllerClient::connect(external_url)
        .await
        .unwrap();
    let mut request = tonic::Request::new(req);
    *request.metadata_mut() = md;
    let resp = client.list_workspaces(request).await.unwrap().into_inner();
    assert_eq!(
        resp.workspaces,
        vec!["ws-alpha".to_string(), "ws-beta".to_string()]
    );

    // Mutate the cached spec.workspaces and call again. The handler
    // re-reads on every call, so the device sees operator edits
    // without re-enrolling.
    verifier.registrations().write().await.insert(
        TEST_EXT_KID.to_string(),
        ClientRegistration {
            verifying_key: p256_vk,
            workspaces: vec!["ws-alpha".to_string()],
        },
    );

    let req2 = ListWorkspacesRequest {};
    let body2 = req2.encode_to_vec();
    let md2 = sign_metadata_no_workspace(
        &p256_sk,
        "/tightbeam.v1.TightbeamController/ListWorkspaces",
        &body2,
        TEST_EXT_KID,
    );
    let mut request2 = tonic::Request::new(req2);
    *request2.metadata_mut() = md2;
    let resp2 = client.list_workspaces(request2).await.unwrap().into_inner();
    assert_eq!(resp2.workspaces, vec!["ws-alpha".to_string()]);
}

#[tokio::test]
async fn external_listener_rejects_list_workspaces_signed_with_unregistered_kid() {
    // Defense check: a syntactically-valid signed ListWorkspaces with a
    // kid the verifier has never seen must be rejected, not silently
    // returning an empty workspace list.
    use tightbeam_proto::ListWorkspacesRequest;

    let (_internal_url, external_url, _registered_sk, _verifier) =
        start_server_with_external_listener().await;

    let attacker_sk = p256::ecdsa::SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
    let req = ListWorkspacesRequest {};
    use prost::Message as _;
    let body_bytes = req.encode_to_vec();
    let md = sign_metadata_no_workspace(
        &attacker_sk,
        "/tightbeam.v1.TightbeamController/ListWorkspaces",
        &body_bytes,
        "ghost-kid",
    );

    let mut client = TightbeamControllerClient::connect(external_url)
        .await
        .unwrap();
    let mut request = tonic::Request::new(req);
    *request.metadata_mut() = md;
    let err = client.list_workspaces(request).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}
