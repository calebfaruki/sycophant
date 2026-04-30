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
        sycophant_scheduling::SchedulingConfig::default(),
    ));
    state
        .set_model_spec(
            "default".into(),
            TightbeamModelSpec {
                format: "anthropic".into(),
                model: "claude-sonnet-4-20250514".into(),
                base_url: "https://api.anthropic.com/v1".into(),
                thinking: None,
                secret: None,
            },
        )
        .await;

    let service = ControllerService::new(state.clone(), None);

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
            job_id: "job-1".into(),
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
                job_id: "job-1".into(),
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
                    structured_json: None,
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
        .turn(TurnRequest {
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
                agent: None,
            }],
            agent: None,
            model: None,
            reply_channel: None,
            role: None,
            response_schema_json: None,
        })
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
                job_id: "job-1".into(),
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
                    structured_json: None,
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
        .turn(TurnRequest {
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
                agent: None,
            }],
            agent: None,
            model: None,
            reply_channel: None,
            role: None,
            response_schema_json: None,
        })
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
async fn system_prompt_persisted_in_conversation() {
    let (url, state) = start_server().await;

    let url_clone = url.clone();

    let llm_job = tokio::spawn(async move {
        let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();

        let assignment = client
            .get_turn(GetTurnRequest {
                job_id: "job-1".into(),
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
                structured_json: None,
            })),
        }];

        client
            .stream_turn_result(stream_turn_result_request("default", chunks))
            .await
            .unwrap();
    });

    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let mut stream = client
        .turn(TurnRequest {
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
                agent: None,
            }],
            agent: None,
            model: None,
            reply_channel: None,
            role: None,
            response_schema_json: None,
        })
        .await
        .unwrap()
        .into_inner();

    while stream.message().await.unwrap().is_some() {}
    llm_job.await.unwrap();

    let ws = state.get_or_create_workspace("default").await;
    let conv = ws.conversation.read().await;
    assert_eq!(conv.system_prompt(), Some("Be helpful."));
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
            structured_json: None,
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
                job_id: "job-1".into(),
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
                structured_json: None,
            })),
        }];
        client
            .stream_turn_result(stream_turn_result_request("default", chunks))
            .await
            .unwrap();
    });

    let mut client = TightbeamControllerClient::connect(url).await.unwrap();
    let mut stream = client
        .turn(TurnRequest {
            system: None,
            tools: vec![],
            messages: vec![],
            agent: None,
            model: None,
            reply_channel: None,
            role: None,
            response_schema_json: None,
        })
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
                job_id: "job-1".into(),
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
                structured_json: None,
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
            .turn(TurnRequest {
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
                    agent: None,
                }],
                agent: None,
                model: None,
                reply_channel: None,
                role: None,
                response_schema_json: None,
            })
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
            structured_json: None,
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
        agent: None,
    }
}

#[tokio::test]
async fn system_agent_turn_response_is_filtered_from_history_for_provider() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (url, state) = start_server().await;

        let url_clone = url.clone();
        let llm_job = tokio::spawn(async move {
            let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();
            let _assignment = client
                .get_turn(GetTurnRequest {
                    job_id: "job-1".into(),
                    model_name: "default".into(),
                })
                .await
                .unwrap()
                .into_inner();
            client
                .stream_turn_result(stream_turn_result_request(
                    "default",
                    vec![complete_chunk("alice")],
                ))
                .await
                .unwrap();
        });

        let mut client = TightbeamControllerClient::connect(url).await.unwrap();
        let mut stream = client
            .turn(TurnRequest {
                system: Some("router prompt".into()),
                tools: vec![],
                messages: vec![user_text_message("Hi, who are you?")],
                agent: Some("system".into()),
                model: None,
                reply_channel: None,
                role: Some(TurnRole::SystemAgent as i32),
                response_schema_json: None,
            })
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
            "raw history must include both user and system_agent_response"
        );
        assert_eq!(raw[0].role, "user");
        assert_eq!(raw[1].role, "assistant");

        let filtered = conv.history_for_provider();
        assert_eq!(
            filtered.len(),
            1,
            "history_for_provider must drop the system_agent_response"
        );
        assert_eq!(
            filtered[0].role, "user",
            "filtered history must end on user"
        );
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn agent_turn_after_system_turn_assignment_excludes_system_response() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let (url, _state) = start_server().await;

        // Phase 1: drive the system_agent turn to completion.
        let url_phase1 = url.clone();
        let llm_phase1 = tokio::spawn(async move {
            let mut client = TightbeamControllerClient::connect(url_phase1)
                .await
                .unwrap();
            let _assignment = client
                .get_turn(GetTurnRequest {
                    job_id: "job-system".into(),
                    model_name: "default".into(),
                })
                .await
                .unwrap()
                .into_inner();
            client
                .stream_turn_result(stream_turn_result_request(
                    "default",
                    vec![complete_chunk("alice")],
                ))
                .await
                .unwrap();
        });

        let mut client = TightbeamControllerClient::connect(url.clone())
            .await
            .unwrap();
        let mut stream = client
            .turn(TurnRequest {
                system: Some("router prompt".into()),
                tools: vec![],
                messages: vec![user_text_message("Hi, who are you?")],
                agent: Some("system".into()),
                model: None,
                reply_channel: None,
                role: Some(TurnRole::SystemAgent as i32),
                response_schema_json: None,
            })
            .await
            .unwrap()
            .into_inner();
        while stream.message().await.unwrap().is_some() {}
        llm_phase1.await.unwrap();

        // Phase 2: agent turn (role=Agent, messages=[]). Verify the assignment the
        // LLM job receives carries only the original user message — last role user,
        // strict alternation preserved. This is the regression test for Mistral.
        let url_phase2 = url.clone();
        let assignment_check = tokio::spawn(async move {
            let mut client = TightbeamControllerClient::connect(url_phase2)
                .await
                .unwrap();
            let assignment = client
                .get_turn(GetTurnRequest {
                    job_id: "job-agent".into(),
                    model_name: "default".into(),
                })
                .await
                .unwrap()
                .into_inner();

            assert_eq!(
                assignment.messages.len(),
                1,
                "agent turn assignment must have exactly the user message; got {} entries",
                assignment.messages.len()
            );
            assert_eq!(
                assignment.messages[0].role, "user",
                "agent turn assignment last role must be user (Mistral compat)"
            );

            // Complete the agent turn so the test can join.
            client
                .stream_turn_result(stream_turn_result_request(
                    "default",
                    vec![complete_chunk("Hi! I'm alice.")],
                ))
                .await
                .unwrap();
        });

        let mut stream = client
            .turn(TurnRequest {
                system: Some("alice prompt".into()),
                tools: vec![],
                messages: vec![],
                agent: Some("alice".into()),
                model: None,
                reply_channel: None,
                role: Some(TurnRole::Agent as i32),
                response_schema_json: None,
            })
            .await
            .unwrap()
            .into_inner();
        while stream.message().await.unwrap().is_some() {}
        assignment_check.await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn turn_with_schema_propagates_to_assignment() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (url, _state) = start_server().await;

        let url_clone = url.clone();
        let llm_job = tokio::spawn(async move {
            let mut client = TightbeamControllerClient::connect(url_clone).await.unwrap();
            let assignment = client
                .get_turn(GetTurnRequest {
                    job_id: "job-1".into(),
                    model_name: "default".into(),
                })
                .await
                .unwrap()
                .into_inner();

            assert_eq!(
                assignment.response_schema_json.as_deref(),
                Some(r#"{"type":"object"}"#),
                "schema must propagate from TurnRequest to TurnAssignment"
            );

            client
                .stream_turn_result(stream_turn_result_request(
                    "default",
                    vec![complete_chunk("alice")],
                ))
                .await
                .unwrap();
        });

        let mut client = TightbeamControllerClient::connect(url).await.unwrap();
        let mut stream = client
            .turn(TurnRequest {
                system: Some("router".into()),
                tools: vec![],
                messages: vec![user_text_message("Hi")],
                agent: Some("system".into()),
                model: None,
                reply_channel: None,
                role: Some(TurnRole::SystemAgent as i32),
                response_schema_json: Some(r#"{"type":"object"}"#.into()),
            })
            .await
            .unwrap()
            .into_inner();
        while stream.message().await.unwrap().is_some() {}
        llm_job.await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn turn_with_schema_and_tools_rejected() {
    let (url, _state) = start_server().await;
    let mut client = TightbeamControllerClient::connect(url).await.unwrap();

    let result = client
        .turn(TurnRequest {
            system: None,
            tools: vec![tightbeam_proto::ToolDefinition {
                name: "bash".into(),
                description: "shell".into(),
                parameters_json: "{}".into(),
            }],
            messages: vec![user_text_message("Hi")],
            agent: Some("system".into()),
            model: None,
            reply_channel: None,
            role: Some(TurnRole::SystemAgent as i32),
            response_schema_json: Some(r#"{"type":"object"}"#.into()),
        })
        .await;

    assert!(
        result.is_err(),
        "schema + tools on system turn must be rejected"
    );
    let status = result.err().unwrap();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}
