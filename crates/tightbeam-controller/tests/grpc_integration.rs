use shared::auth::TokenVerifier;
use std::collections::HashMap;
use std::sync::Arc;
use tightbeam_controller::conversation::ConversationLog;
use tightbeam_controller::crd::TightbeamModelSpec;
use tightbeam_controller::grpc::ControllerService;
use tightbeam_controller::state::ControllerState;
use tightbeam_proto::tightbeam_controller_client::TightbeamControllerClient;
use tightbeam_proto::tightbeam_controller_server::TightbeamControllerServer;
use tightbeam_proto::{
    content_block, turn_event, turn_result_chunk, ContentBlock, ContentDelta, GetTurnRequest,
    ListModelsRequest, StopReason, TextBlock, ToolCall, ToolUseInput, ToolUseStart, TurnComplete,
    TurnRequest, TurnResultChunk, TurnRole,
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

/// Wrap a request body with a dummy `Authorization: Bearer test` header.
/// Required for any RPC that goes through `verify_workspace` (turn,
/// subscribe). The token contents are ignored by `FixedWorkspaceVerifier`.
fn authed<T>(inner: T) -> tonic::Request<T> {
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
    let mut workspace_convs = HashMap::new();
    workspace_convs.insert(
        "default".to_string(),
        ConversationLog::new(&log_dir.join("default")),
    );
    let state = Arc::new(ControllerState::new(
        workspace_convs,
        log_dir,
        None,
        "default".into(),
        "http://localhost:9090".into(),
        "ghcr.io/test/llm-job:latest".into(),
        shared::scheduling::SchedulingConfig::default(),
    ));
    state
        .set_model_spec(
            "default".into(),
            TightbeamModelSpec {
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
            tightbeam_controller::crd::TightbeamProviderSpec {
                format: "anthropic".into(),
                base_url: Some("https://api.anthropic.com/v1".into()),
                secret: tightbeam_controller::crd::ProviderSecret {
                    name: "anthropic-key".into(),
                    key: None,
                },
            },
        )
        .await;

    let verifier: Arc<dyn TokenVerifier> = Arc::new(FixedWorkspaceVerifier("default".to_string()));
    let service = ControllerService::new(state.clone(), Some(verifier));

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
    request
}

#[tokio::test]
async fn list_models_returns_empty() {
    let (url, _state) = start_server().await;
    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let response = client
        .list_models(ListModelsRequest {})
        .await
        .unwrap()
        .into_inner();

    assert!(response.models.is_empty());
}

#[tokio::test]
async fn get_turn_returns_unimplemented_when_no_pending() {
    let (url, _state) = start_server().await;
    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        client.get_turn(GetTurnRequest {
            model_name: "default".into(),
        }),
    )
    .await;

    assert!(result.is_err(), "GetTurn should block when no turn pending");
}

#[tokio::test]
async fn end_to_end_turn_with_text_response() {
    let (url, state) = start_server().await;

    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();

        let assignment = client
            .get_turn(GetTurnRequest {
                model_name: "default".into(),
            })
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
        .turn(authed(TurnRequest {
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
    let conv = ws.conversation.read().await;
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
            .get_turn(GetTurnRequest {
                model_name: "default".into(),
            })
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
        .turn(authed(TurnRequest {
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
    let conv = ws.conversation.read().await;
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
            .get_turn(GetTurnRequest {
                model_name: "default".into(),
            })
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
        .turn(authed(TurnRequest {
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
async fn turn_with_empty_messages_still_works() {
    let (url, state) = start_server().await;
    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();
        let _assignment = client
            .get_turn(GetTurnRequest {
                model_name: "default".into(),
            })
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
        .turn(authed(TurnRequest {
            system: None,
            tools: vec![],
            messages: vec![],
            model: None,
            reply_channel: None,
            role: None,
            correlation_id: None,
        }))
        .await
        .unwrap()
        .into_inner();

    while stream.message().await.unwrap().is_some() {}
    llm_job.await.unwrap();

    let ws = state.get_or_create_workspace("default").await;
    let conv = ws.conversation.read().await;
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
            .get_turn(GetTurnRequest {
                model_name: "default".into(),
            })
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
            .turn(authed(TurnRequest {
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
                .get_turn(GetTurnRequest {
                    model_name: "default".into(),
                })
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
            .turn(authed(TurnRequest {
                system: Some("delegate prompt".into()),
                tools: vec![],
                messages: vec![user_text_message("delegate query")],
                model: None,
                reply_channel: None,
                role: Some(TurnRole::Delegate as i32),
                correlation_id: Some("call-xyz".into()),
            }))
            .await
            .unwrap()
            .into_inner();

        while stream.message().await.unwrap().is_some() {}
        llm_job.await.unwrap();

        let ws = state.get_or_create_workspace("default").await;
        let conv = ws.conversation.read().await;
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
                TightbeamModelSpec {
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
                .get_turn(GetTurnRequest {
                    model_name: "smart".into(),
                })
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
            .turn(authed(TurnRequest {
                system: Some(raw.into()),
                tools: vec![],
                messages: vec![user_text_message("hi")],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
            }))
            .await
            .unwrap()
            .into_inner();
        while stream.message().await.unwrap().is_some() {}
        llm_job.await.unwrap();

        // The audit hash is computed on the pre-strip value so external
        // `sha256sum` of the canonical file matches directly.
        let ws = state.get_or_create_workspace("default").await;
        let conv = ws.conversation.read().await;
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
                    .get_turn(GetTurnRequest {
                        model_name: "default".into(),
                    })
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
            .turn(authed(TurnRequest {
                system: Some("ENTRYPOINT".into()),
                tools: vec![],
                messages: vec![user_text_message("hello")],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
            }))
            .await
            .unwrap()
            .into_inner();
        while s1.message().await.unwrap().is_some() {}

        // Turn 2: delegate call (different system).
        let mut s2 = client
            .turn(authed(TurnRequest {
                system: Some("DELEGATE_PROMPT".into()),
                tools: vec![],
                messages: vec![user_text_message("delegate query")],
                model: None,
                reply_channel: None,
                role: Some(TurnRole::Delegate as i32),
                correlation_id: Some("d1".into()),
            }))
            .await
            .unwrap()
            .into_inner();
        while s2.message().await.unwrap().is_some() {}

        // Turn 3: orchestrator continuation. Must carry ENTRYPOINT, not
        // DELEGATE_PROMPT — that's the regression we're guarding against.
        let mut s3 = client
            .turn(authed(TurnRequest {
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
            }))
            .await
            .unwrap()
            .into_inner();
        while s3.message().await.unwrap().is_some() {}

        llm_job.await.unwrap();

        let ws = state.get_or_create_workspace("default").await;
        let conv = ws.conversation.read().await;
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
                TightbeamModelSpec {
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
                .get_turn(GetTurnRequest {
                    model_name: "default".into(),
                })
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
            .turn(authed(TurnRequest {
                system: Some("plain prompt with no frontmatter".into()),
                tools: vec![],
                messages: vec![user_text_message("hi")],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
            }))
            .await
            .unwrap()
            .into_inner();
        while stream.message().await.unwrap().is_some() {}
        llm_job.await.unwrap();

        let ws = state.get_or_create_workspace("default").await;
        let conv = ws.conversation.read().await;
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
            .get_turn(GetTurnRequest {
                model_name: "".into(),
            })
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
            .turn(authed(TurnRequest {
                system: Some("plain prompt".into()),
                tools: vec![],
                messages: vec![user_text_message("hi")],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
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
