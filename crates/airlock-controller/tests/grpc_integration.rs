use std::sync::Arc;

use airlock_controller::crd::{Chamber, ChamberSpec};
use airlock_controller::grpc::ControllerService;
use airlock_controller::registry::{ArgDecl, ArgType};
use airlock_controller::state::{ControllerState, RegisteredTool, WorkspaceBindings};
use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::airlock_controller_server::AirlockControllerServer;
use airlock_proto::{AwaitToolResultRequest, GetToolCallRequest, SendToolResultRequest};
use proto_common::{CallToolRequest, WatchToolsRequest};
use tonic::transport::Server;

async fn start_server() -> (String, Arc<ControllerState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");

    let state = ControllerState::new(
        None,
        String::new(),
        String::new(),
        shared::scheduling::SchedulingConfig::default(),
    );
    let service = ControllerService::new(state.clone(), None, WorkspaceBindings::empty());

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        Server::builder()
            .add_service(AirlockControllerServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (url, state)
}

fn make_chamber(name: &str) -> Chamber {
    Chamber::new(
        name,
        ChamberSpec {
            image: None,
            credentials: vec![],
            egress: vec![],
            keepalive: false,
        },
    )
}

async fn register_tools(state: &ControllerState, chamber: &str, tools: Vec<(&str, &str)>) {
    let registered: Vec<RegisteredTool> = tools
        .into_iter()
        .map(|(name, desc)| RegisteredTool {
            name: name.to_string(),
            chamber_name: chamber.to_string(),
            description: desc.to_string(),
            image: "test:latest".to_string(),
            args: vec![],
        })
        .collect();
    state.set_tools_for_chamber(chamber, registered).await;
}

async fn register_tool_with_args(
    state: &ControllerState,
    chamber: &str,
    name: &str,
    desc: &str,
    args: Vec<ArgDecl>,
) {
    state
        .set_tools_for_chamber(
            chamber,
            vec![RegisteredTool {
                name: name.to_string(),
                chamber_name: chamber.to_string(),
                description: desc.to_string(),
                image: "test:latest".to_string(),
                args,
            }],
        )
        .await;
}

#[tokio::test]
async fn watch_tools_initial_snapshot_over_grpc() {
    use tokio_stream::StreamExt;

    let (url, state) = start_server().await;
    register_tools(&state, "test-chamber", vec![("git-push", "Push commits")]).await;

    let mut client = AirlockControllerClient::connect(url).await.unwrap();
    let mut stream = client
        .watch_tools(WatchToolsRequest {})
        .await
        .unwrap()
        .into_inner();

    let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("watch_tools must yield initial snapshot")
        .expect("stream not closed")
        .expect("ok response");

    assert_eq!(first.tools.len(), 1);
    assert_eq!(first.tools[0].name, "git-push");
    assert_eq!(first.tools[0].description, "Push commits");
}

#[tokio::test]
async fn get_tool_call_blocks_over_grpc() {
    let (url, _state) = start_server().await;
    let mut client = AirlockControllerClient::connect(url).await.unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        client.get_tool_call(GetToolCallRequest {
            job_id: "job-1".into(),
            tool_name: "echo".into(),
        }),
    )
    .await;

    assert!(
        result.is_err(),
        "GetToolCall should block when no calls pending"
    );
}

#[tokio::test]
async fn call_tool_round_trip_over_grpc() {
    let (url, state) = start_server().await;
    register_tool_with_args(
        &state,
        "test-chamber",
        "echo",
        "Echo tool",
        vec![ArgDecl {
            name: "message".into(),
            ty: ArgType::String,
            required: true,
            env: "MESSAGE".into(),
            description: None,
        }],
    )
    .await;
    state
        .set_chamber("test-chamber".into(), make_chamber("test-chamber"))
        .await;

    let agent_url = url.clone();

    let runtime = tokio::spawn(async move {
        let mut client = AirlockControllerClient::connect(agent_url).await.unwrap();

        let assignment = client
            .get_tool_call(GetToolCallRequest {
                job_id: "job-1".into(),
                tool_name: "echo".into(),
            })
            .await
            .unwrap()
            .into_inner();

        assert_eq!(
            assignment.args.get("MESSAGE"),
            Some(&"hello world".to_string())
        );

        client
            .send_tool_result(SendToolResultRequest {
                call_id: assignment.call_id,
                content: proto_common::text_content("hello world\n"),
                is_error: false,
                exit_code: 0,
            })
            .await
            .unwrap();
    });

    let mut client = AirlockControllerClient::connect(url).await.unwrap();
    let handle = client
        .begin_tool_call(CallToolRequest {
            name: "echo".into(),
            input_json: r#"{"message":"hello world"}"#.into(),
        })
        .await
        .unwrap()
        .into_inner();
    let resp = client
        .await_tool_result(AwaitToolResultRequest {
            call_id: handle.call_id,
        })
        .await
        .unwrap()
        .into_inner();

    assert_eq!(proto_common::content_text(&resp.content), "hello world\n");
    assert!(!resp.is_error);

    runtime.await.unwrap();
}

// An image produced in the chamber rides the content-part list
// on `SendToolResult` (the image is a part of `content`, not a separate image
// field on that leg), and it comes back on the tool answer as an image part
// carrying its media type and its exact bytes — carried through every internal
// leg untouched.
//
// Materiality: this requires the wire widening (`SendToolResultRequest.content`
// and `CallToolResponse.content`) plus the controller building its internal
// result from `req.content` and the answer from `result.content`. A mutant that
// drops the image, converts it to a text part, reads a sibling image field, or
// loses the bytes reds the assertions below; reverting the leg to a `string
// output` fails to compile.
#[tokio::test]
async fn chamber_image_result_is_carried_through_to_the_answer_as_an_image_part() {
    let (url, state) = start_server().await;
    register_tool_with_args(
        &state,
        "test-chamber",
        "preview",
        "Preview tool",
        vec![ArgDecl {
            name: "path".into(),
            ty: ArgType::String,
            required: true,
            env: "PATH_ARG".into(),
            description: None,
        }],
    )
    .await;
    state
        .set_chamber("test-chamber".into(), make_chamber("test-chamber"))
        .await;

    // A PNG signature plus a few payload bytes — enough to prove the exact
    // bytes survive every hop without truncation or re-encoding.
    let png: Vec<u8> = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];
    let runtime_png = png.clone();
    let runtime_url = url.clone();

    let runtime = tokio::spawn(async move {
        let mut client = AirlockControllerClient::connect(runtime_url).await.unwrap();
        let assignment = client
            .get_tool_call(GetToolCallRequest {
                job_id: "job-1".into(),
                tool_name: "preview".into(),
            })
            .await
            .unwrap()
            .into_inner();

        client
            .send_tool_result(SendToolResultRequest {
                call_id: assignment.call_id,
                content: vec![proto_common::ContentBlock {
                    block: Some(proto_common::content_block::Block::Image(
                        proto_common::ImageBlock {
                            media_type: "image/png".into(),
                            data: runtime_png,
                        },
                    )),
                }],
                is_error: false,
                exit_code: 0,
            })
            .await
            .unwrap();
    });

    let mut client = AirlockControllerClient::connect(url).await.unwrap();
    let handle = client
        .begin_tool_call(CallToolRequest {
            name: "preview".into(),
            input_json: r#"{"path":"/workspace/x.pdf"}"#.into(),
        })
        .await
        .unwrap()
        .into_inner();
    let resp = client
        .await_tool_result(AwaitToolResultRequest {
            call_id: handle.call_id,
        })
        .await
        .unwrap()
        .into_inner();

    assert!(!resp.is_error);
    assert_eq!(
        resp.content.len(),
        1,
        "the answer carries exactly the image part the chamber produced"
    );
    match resp.content[0].block.as_ref() {
        Some(proto_common::content_block::Block::Image(img)) => {
            assert_eq!(img.media_type, "image/png", "media type survives every leg");
            assert_eq!(img.data, png, "the image bytes survive every leg untouched");
        }
        other => panic!("expected an image part on the answer, got {other:?}"),
    }

    runtime.await.unwrap();
}

#[tokio::test]
async fn call_tool_for_same_tool_runs_in_parallel() {
    let (url, state) = start_server().await;
    register_tool_with_args(
        &state,
        "test-chamber",
        "echo",
        "Echo tool",
        vec![ArgDecl {
            name: "message".into(),
            ty: ArgType::String,
            required: true,
            env: "MESSAGE".into(),
            description: None,
        }],
    )
    .await;
    state
        .set_chamber("test-chamber".into(), make_chamber("test-chamber"))
        .await;

    let runtime_url = url.clone();

    // Runtime drains TWO get_tool_call assignments BEFORE sending either
    // result. If the dispatch guard re-extends across enqueue/result_rx,
    // the second call's enqueue never happens and the second drain
    // times out.
    let runtime = tokio::spawn(async move {
        let mut client = AirlockControllerClient::connect(runtime_url).await.unwrap();

        let first = client
            .get_tool_call(GetToolCallRequest {
                job_id: "job-1".into(),
                tool_name: "echo".into(),
            })
            .await
            .unwrap()
            .into_inner();

        let second = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.get_tool_call(GetToolCallRequest {
                job_id: "job-2".into(),
                tool_name: "echo".into(),
            }),
        )
        .await
        .expect("second get_tool_call must resolve while first call is still pending")
        .unwrap()
        .into_inner();

        client
            .send_tool_result(SendToolResultRequest {
                call_id: first.call_id,
                content: proto_common::text_content("first\n"),
                is_error: false,
                exit_code: 0,
            })
            .await
            .unwrap();

        client
            .send_tool_result(SendToolResultRequest {
                call_id: second.call_id,
                content: proto_common::text_content("second\n"),
                is_error: false,
                exit_code: 0,
            })
            .await
            .unwrap();
    });

    let mut client_a = AirlockControllerClient::connect(url.clone()).await.unwrap();
    let mut client_b = AirlockControllerClient::connect(url).await.unwrap();

    // Both begins enqueue (releasing the per-tool dispatch guard between them)
    // before either result is sent, so the runtime can drain both assignments.
    let handle_a = client_a
        .begin_tool_call(CallToolRequest {
            name: "echo".into(),
            input_json: r#"{"message":"first"}"#.into(),
        })
        .await
        .unwrap()
        .into_inner();
    let handle_b = client_b
        .begin_tool_call(CallToolRequest {
            name: "echo".into(),
            input_json: r#"{"message":"second"}"#.into(),
        })
        .await
        .unwrap()
        .into_inner();

    let call_a = client_a.await_tool_result(AwaitToolResultRequest {
        call_id: handle_a.call_id,
    });
    let call_b = client_b.await_tool_result(AwaitToolResultRequest {
        call_id: handle_b.call_id,
    });

    let (resp_a, resp_b) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(call_a, call_b)
    })
    .await
    .expect("both await_tool_result futures must resolve");

    let out_a = proto_common::content_text(&resp_a.unwrap().into_inner().content);
    let out_b = proto_common::content_text(&resp_b.unwrap().into_inner().content);

    let mut outputs = [out_a, out_b];
    outputs.sort();
    assert_eq!(outputs, ["first\n".to_string(), "second\n".to_string()]);

    runtime.await.unwrap();
}
