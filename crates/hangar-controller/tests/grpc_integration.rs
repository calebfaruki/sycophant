use hangar_controller::crd::ModelSpec;
use hangar_controller::grpc::ControllerService;
use hangar_controller::state::ControllerState;
use hangar_proto::hangar_controller_client::HangarControllerClient;
use hangar_proto::hangar_controller_server::HangarControllerServer;
use hangar_proto::{
    content_block, turn_event, turn_result_chunk, ContentBlock, ContentDelta, GetTurnRequest,
    StopReason, TextBlock, ToolCall, ToolUseInput, ToolUseStart, TurnComplete, TurnRequest,
    TurnResultChunk,
};
use shared::auth::TokenVerifier;
use std::sync::Arc;
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

    let state = Arc::new(ControllerState::new(
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
                provider_ref: hangar_controller::crd::ProviderRef {
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
            hangar_controller::crd::ProviderSpec {
                format: "anthropic".into(),
                base_url: Some("https://api.anthropic.com/v1".into()),
                secret: hangar_controller::crd::ProviderSecret {
                    name: "anthropic-key".into(),
                    key: None,
                },
            },
        )
        .await;

    let pair = hangar_controller::grpc::InternalVerifierPair {
        transponder: Arc::new(FixedWorkspaceVerifier("default".to_string())),
        llm: Arc::new(FixedWorkspaceVerifier("default".to_string())),
    };
    let service = ControllerService::internal(state.clone(), Some(pair));

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        Server::builder()
            .layer(hangar_controller::audience_layer::RequiredAudienceLayer)
            .add_service(HangarControllerServer::new(service))
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
        .insert("x-hangar-model", model.parse().unwrap());
    // verify_workspace requires a bearer token; FixedWorkspaceVerifier
    // ignores the token value so any non-empty string suffices.
    request
        .metadata_mut()
        .insert("authorization", "Bearer test".parse().unwrap());
    request
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

    let state = Arc::new(ControllerState::new(
        None,
        "default".into(),
        "http://localhost:9090".into(),
        "ghcr.io/test/llm-job:latest".into(),
        shared::scheduling::SchedulingConfig::default(),
    ));

    let pair = hangar_controller::grpc::InternalVerifierPair {
        transponder: Arc::new(FixedWorkspaceVerifier("mf-tag".to_string())),
        llm: Arc::new(FixedWorkspaceVerifier("llm-tag".to_string())),
    };
    let service = ControllerService::internal(state.clone(), Some(pair));

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        Server::builder()
            .layer(hangar_controller::audience_layer::RequiredAudienceLayer)
            .add_service(HangarControllerServer::new(service))
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
    let mut client = HangarControllerClient::connect(url).await.unwrap();

    // Enqueue a pending turn for workspace "llm-tag" so the GetTurn
    // workspace-match check passes only if the llm slot ran.
    state
        .set_model_spec(
            "default".into(),
            ModelSpec {
                provider_ref: hangar_controller::crd::ProviderRef {
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
            hangar_controller::crd::ProviderSpec {
                format: "anthropic".into(),
                base_url: Some("https://api.anthropic.com/v1".into()),
                secret: hangar_controller::crd::ProviderSecret {
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
            hangar_controller::state::PendingTurn {
                assignment: hangar_proto::TurnAssignment {
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
        .get_turn(bearer_authed(hangar_proto::GetTurnRequest {
            model_name: "default".into(),
        }))
        .await
        .expect("GetTurn must succeed — llm slot must be selected for GetTurn method");
    let _ = resp.into_inner();
}

#[tokio::test]
async fn get_turn_returns_unimplemented_when_no_pending() {
    let (url, _state) = start_server().await;
    let mut client = HangarControllerClient::connect(url).await.unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        client.get_turn(bearer_authed(GetTurnRequest {
            model_name: "default".into(),
        })),
    )
    .await;

    assert!(result.is_err(), "GetTurn should block when no turn pending");
}

#[tokio::test]
async fn end_to_end_turn_with_text_response() {
    let (url, _state) = start_server().await;

    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = HangarControllerClient::connect(url_clone).await.unwrap();

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

    let mut client = HangarControllerClient::connect(url).await.unwrap();

    let mut response_stream = client
        .turn(bearer_authed(TurnRequest {
            system: Some("You are a test assistant.".into()),
            tools: vec![],
            messages: vec![hangar_proto::Message {
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
}

#[tokio::test]
async fn end_to_end_turn_with_tool_use() {
    let (url, _state) = start_server().await;

    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = HangarControllerClient::connect(url_clone).await.unwrap();

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

    let mut client = HangarControllerClient::connect(url).await.unwrap();

    let mut response_stream = client
        .turn(bearer_authed(TurnRequest {
            system: None,
            tools: vec![],
            messages: vec![hangar_proto::Message {
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
}

#[tokio::test]
async fn assignment_carries_system_from_request() {
    let (url, _state) = start_server().await;

    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = HangarControllerClient::connect(url_clone).await.unwrap();

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

    let mut client = HangarControllerClient::connect(url).await.unwrap();

    let mut stream = client
        .turn(bearer_authed(TurnRequest {
            system: Some("Be helpful.".into()),
            tools: vec![],
            messages: vec![hangar_proto::Message {
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
    let mut client = HangarControllerClient::connect(url).await.unwrap();

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
                provider_ref: hangar_controller::crd::ProviderRef {
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

    let mut client = HangarControllerClient::connect(url).await.unwrap();
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
    let (url, _state) = start_server().await;
    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = HangarControllerClient::connect(url_clone).await.unwrap();
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

    let mut client = HangarControllerClient::connect(url).await.unwrap();
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
}

#[tokio::test]
async fn get_turn_before_turn_delivers() {
    let (url, _state) = start_server().await;

    let url_for_job = url.clone();
    let url_for_transponder = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = HangarControllerClient::connect(url_for_job).await.unwrap();

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
        let mut client = HangarControllerClient::connect(url_for_transponder)
            .await
            .unwrap();

        let mut stream = client
            .turn(bearer_authed(TurnRequest {
                system: None,
                tools: vec![],
                messages: vec![hangar_proto::Message {
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

fn user_text_message(text: &str) -> hangar_proto::Message {
    hangar_proto::Message {
        role: "user".into(),
        content: vec![ContentBlock {
            block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
        }],
        tool_calls: vec![],
        tool_call_id: None,
        is_error: None,
    }
}

/// `get_turn` rejects `GetTurnRequest` with an empty `model_name`. That call
/// shape used to silently fall back to `"default"`; now it errors.
#[tokio::test]
async fn get_turn_errors_when_model_name_empty() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (url, _state) = start_server().await;
        let mut client = HangarControllerClient::connect(url).await.unwrap();

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

        let mut client = HangarControllerClient::connect(url).await.unwrap();
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
    let mut client = HangarControllerClient::connect(url).await.unwrap();
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
