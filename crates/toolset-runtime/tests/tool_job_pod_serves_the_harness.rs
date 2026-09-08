//! Integration test for the reversed tool-dispatch direction: the tool-job pod
//! is a gRPC SERVER. The harness dials it and sends the tool-call assignment as
//! the first `RunToolCommand` on a held `ToolJob.Run` stream; the pod runs the
//! assigned tool and streams its result frames back on the SAME connection.
//!
//! No `ToolsetController` server exists anywhere in this test. If the runtime
//! still held an outbound client that dialed the harness for its assignment (the
//! pre-flip design), it could never obtain a call and this test could not
//! complete. That absence is the load-bearing check for "the pod holds no client
//! that dials out to the harness".
//!
//! Materiality: making the runtime a client again (no `ToolJob` server) leaves
//! nothing for `ToolJobClient::connect` to reach, so `run` never returns frames.
//! Dropping the assignment-first read makes the pod run no tool, so the target
//! file is never written.

use std::collections::HashMap;
use std::time::Duration;

use proto_common::tool_result_frame::Frame;
use tokio_stream::StreamExt;
use tonic::transport::Server;

use toolset_proto::run_tool_command::Command;
use toolset_proto::tool_job_client::ToolJobClient;
use toolset_proto::tool_job_server::ToolJobServer;
use toolset_proto::{RunToolCommand, ToolCallAssignment};

#[tokio::test]
async fn the_pod_serves_run_executes_the_assignment_and_streams_frames_back() {
    // Reserve then free an ephemeral port; the client retries over the gap.
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reserve.local_addr().unwrap();
    drop(reserve);

    // The pod is the server. It runs the `Write` builtin named in its config.
    let service = toolset_runtime::ToolJobService::new("Write");
    tokio::spawn(async move {
        Server::builder()
            .add_service(ToolJobServer::new(service))
            .serve(addr)
            .await
            .unwrap();
    });

    let mut client = loop {
        match ToolJobClient::connect(format!("http://{addr}")).await {
            Ok(c) => break c,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("written.txt");
    let mut args = HashMap::new();
    args.insert("path".to_string(), target.to_str().unwrap().to_string());
    args.insert("content".to_string(), "job-served-this".to_string());

    // The assignment is the first (and here only) message on the outbound half.
    let first = RunToolCommand {
        command: Some(Command::Assignment(ToolCallAssignment {
            call_id: "call-1".to_string(),
            working_dir: dir.path().to_str().unwrap().to_string(),
            args,
        })),
    };

    let response = client
        .run(tokio_stream::once(first))
        .await
        .expect("the pod must serve Run");
    let mut frames = response.into_inner();

    let mut saw_terminal = false;
    while let Some(frame) = frames.next().await {
        let frame = frame.expect("a frame");
        if matches!(frame.frame, Some(Frame::Complete(_))) {
            saw_terminal = true;
        }
    }

    assert!(
        saw_terminal,
        "the pod must stream a terminal frame back on the Run connection"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "job-served-this",
        "the pod must execute the tool named in the first RunToolCommand assignment"
    );
}
