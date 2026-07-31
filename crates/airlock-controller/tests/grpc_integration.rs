use std::sync::Arc;

use airlock_controller::crd::{Chamber, ChamberSpec};
use airlock_controller::grpc::ControllerService;
use airlock_controller::registry::{ArgDecl, ArgType};
use airlock_controller::state::{ControllerState, RegisteredTool, WorkspaceBindings};
use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::airlock_controller_server::AirlockControllerServer;
use airlock_proto::{AwaitToolResultRequest, GetToolCallRequest};
use proto_common::tool_result_frame::Frame;
use proto_common::{
    CallToolRequest, ToolComplete, ToolOutcome, ToolResultFrame, WatchToolsRequest,
};
use tonic::transport::Server;

/// Build a client-stream request of frames carrying the call_id on the
/// `x-airlock-call-id` metadata header — the shape the runtime sends.
fn frame_request(
    call_id: &str,
    frames: Vec<ToolResultFrame>,
) -> tonic::Request<impl futures::Stream<Item = ToolResultFrame>> {
    let mut request = tonic::Request::new(futures::stream::iter(frames));
    request
        .metadata_mut()
        .insert("x-airlock-call-id", call_id.parse().unwrap());
    request
}

fn stdout_frame(text: &str) -> ToolResultFrame {
    ToolResultFrame {
        frame: Some(Frame::Stdout(text.into())),
    }
}

fn image_frame(media_type: &str, data: Vec<u8>) -> ToolResultFrame {
    ToolResultFrame {
        frame: Some(Frame::Image(proto_common::ImageBlock {
            media_type: media_type.into(),
            data,
        })),
    }
}

fn complete_frame(is_error: bool, exit_code: i32) -> ToolResultFrame {
    let outcome = if is_error {
        ToolOutcome::Failed
    } else {
        ToolOutcome::Done
    };
    ToolResultFrame {
        frame: Some(Frame::Complete(ToolComplete {
            outcome: outcome as i32,
            exit_code,
        })),
    }
}

/// Drain an `AwaitToolResult` server-stream to EOF, collecting its frames.
async fn drain_result_stream(
    mut stream: tonic::Streaming<ToolResultFrame>,
) -> Vec<ToolResultFrame> {
    use tokio_stream::StreamExt;
    let mut out = Vec::new();
    while let Some(frame) = stream.next().await {
        out.push(frame.expect("frame stream must not error"));
    }
    out
}

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

    let mut client = AirlockControllerClient::connect(url.clone()).await.unwrap();
    let handle = client
        .begin_tool_call(CallToolRequest {
            name: "echo".into(),
            input_json: r#"{"message":"hello world"}"#.into(),
            conversation_id: String::new(),
        })
        .await
        .unwrap()
        .into_inner();

    // Runtime leg: claim the assignment.
    let mut runtime_client = AirlockControllerClient::connect(url).await.unwrap();
    let assignment = runtime_client
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

    // The transponder opens the result stream (taking the parked receiver)
    // before the runtime streams its frames.
    let result_stream = client
        .await_tool_result(AwaitToolResultRequest {
            call_id: handle.call_id.clone(),
        })
        .await
        .unwrap()
        .into_inner();

    // Runtime client-streams its typed output frames.
    runtime_client
        .stream_tool_result(frame_request(
            &assignment.call_id,
            vec![stdout_frame("hello world\n"), complete_frame(false, 0)],
        ))
        .await
        .unwrap();

    let frames = drain_result_stream(result_stream).await;
    assert!(
        matches!(frames.first().and_then(|f| f.frame.as_ref()), Some(Frame::Stdout(s)) if s == "hello world\n"),
        "the stdout frame arrives first, got {frames:?}"
    );
    match frames.last().and_then(|f| f.frame.as_ref()) {
        Some(Frame::Complete(c)) => assert_eq!(c.outcome(), ToolOutcome::Done),
        other => panic!("the last frame must be the terminal, got {other:?}"),
    }
}

// An image produced in the chamber rides its own `image` frame on the result
// stream and comes back on the `AwaitToolResult` stream carrying its media type
// and its exact bytes — carried through every internal leg untouched.
//
// Materiality: this requires the streaming legs (`StreamToolResult` client-
// stream + `AwaitToolResult` server-stream) plus the controller forwarding each
// frame unchanged. A mutant that drops the image frame, converts it to a text
// frame, or loses the bytes reds the assertions below.
#[tokio::test]
async fn chamber_image_result_is_carried_through_to_the_answer_as_an_image_frame() {
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

    let mut client = AirlockControllerClient::connect(url.clone()).await.unwrap();
    let handle = client
        .begin_tool_call(CallToolRequest {
            name: "preview".into(),
            input_json: r#"{"path":"/workspace/x.pdf"}"#.into(),
            conversation_id: String::new(),
        })
        .await
        .unwrap()
        .into_inner();

    let mut runtime_client = AirlockControllerClient::connect(url).await.unwrap();
    let assignment = runtime_client
        .get_tool_call(GetToolCallRequest {
            job_id: "job-1".into(),
            tool_name: "preview".into(),
        })
        .await
        .unwrap()
        .into_inner();

    let result_stream = client
        .await_tool_result(AwaitToolResultRequest {
            call_id: handle.call_id.clone(),
        })
        .await
        .unwrap()
        .into_inner();

    runtime_client
        .stream_tool_result(frame_request(
            &assignment.call_id,
            vec![
                image_frame("image/png", png.clone()),
                complete_frame(false, 0),
            ],
        ))
        .await
        .unwrap();

    let frames = drain_result_stream(result_stream).await;
    let image = frames
        .iter()
        .find_map(|f| match f.frame.as_ref() {
            Some(Frame::Image(img)) => Some(img),
            _ => None,
        })
        .expect("the answer stream carries the image frame the chamber produced");
    assert_eq!(
        image.media_type, "image/png",
        "media type survives every leg"
    );
    assert_eq!(
        image.data, png,
        "the image bytes survive every leg untouched"
    );
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

    let mut client_a = AirlockControllerClient::connect(url.clone()).await.unwrap();
    let mut client_b = AirlockControllerClient::connect(url.clone()).await.unwrap();

    // Both begins enqueue (releasing the per-tool dispatch guard between them)
    // before either result flows, so the runtime can drain both assignments.
    let handle_a = client_a
        .begin_tool_call(CallToolRequest {
            name: "echo".into(),
            input_json: r#"{"message":"first"}"#.into(),
            conversation_id: String::new(),
        })
        .await
        .unwrap()
        .into_inner();
    let handle_b = client_b
        .begin_tool_call(CallToolRequest {
            name: "echo".into(),
            input_json: r#"{"message":"second"}"#.into(),
            conversation_id: String::new(),
        })
        .await
        .unwrap()
        .into_inner();

    // Runtime drains TWO get_tool_call assignments BEFORE streaming either
    // result. If the dispatch guard re-extends across enqueue/result_rx, the
    // second call's enqueue never happens and the second drain times out.
    let mut runtime_client = AirlockControllerClient::connect(url).await.unwrap();
    let first = runtime_client
        .get_tool_call(GetToolCallRequest {
            job_id: "job-1".into(),
            tool_name: "echo".into(),
        })
        .await
        .unwrap()
        .into_inner();
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runtime_client.get_tool_call(GetToolCallRequest {
            job_id: "job-2".into(),
            tool_name: "echo".into(),
        }),
    )
    .await
    .expect("second get_tool_call must resolve while first call is still pending")
    .unwrap()
    .into_inner();

    // Open both result streams (taking the parked receivers) before frames flow.
    let stream_a = client_a
        .await_tool_result(AwaitToolResultRequest {
            call_id: handle_a.call_id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    let stream_b = client_b
        .await_tool_result(AwaitToolResultRequest {
            call_id: handle_b.call_id.clone(),
        })
        .await
        .unwrap()
        .into_inner();

    // Map each assignment's call_id back to the message it carried, so we stream
    // the matching output regardless of dispatch order.
    let label = |call_id: &str| -> &'static str {
        if call_id == handle_a.call_id {
            "first\n"
        } else {
            "second\n"
        }
    };
    runtime_client
        .stream_tool_result(frame_request(
            &first.call_id,
            vec![
                stdout_frame(label(&first.call_id)),
                complete_frame(false, 0),
            ],
        ))
        .await
        .unwrap();
    runtime_client
        .stream_tool_result(frame_request(
            &second.call_id,
            vec![
                stdout_frame(label(&second.call_id)),
                complete_frame(false, 0),
            ],
        ))
        .await
        .unwrap();

    let (frames_a, frames_b) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(drain_result_stream(stream_a), drain_result_stream(stream_b))
    })
    .await
    .expect("both result streams must drain");

    let text = |frames: &[ToolResultFrame]| -> String {
        frames
            .iter()
            .filter_map(|f| match f.frame.as_ref() {
                Some(Frame::Stdout(s)) => Some(s.clone()),
                _ => None,
            })
            .collect()
    };
    let mut outputs = [text(&frames_a), text(&frames_b)];
    outputs.sort();
    assert_eq!(outputs, ["first\n".to_string(), "second\n".to_string()]);
}
