//! Integration test for the harness-dialed inference direction: the
//! inference-runtime pod is a gRPC SERVER. The harness dials it and sends the
//! model-call assignment as the first `RunInferenceCommand` on a held
//! `InferenceJob.Run` stream; the pod runs the model call and streams the
//! model's `TurnEvent`s back on the SAME connection.
//!
//! No `ToolsetController` server exists anywhere in this test. If the runtime
//! still held an outbound client that long-polled the controller for its
//! assignment (the pre-flip pull design), it could never obtain a turn and this
//! test could not complete. That absence is the load-bearing check for "the pod
//! holds no client that dials out to the harness".
//!
//! Materiality: making the runtime a client again (no `InferenceJob` server)
//! leaves nothing for `InferenceJobClient::connect` to reach, so `run` never
//! returns events. Dropping the assignment-first read makes the pod run no model
//! call, so the fake provider is never consumed.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use model_provider::{LlmProvider, ProviderConfig, StreamEvent};
use proto_common::{Message, ToolDefinition};
use tokio_stream::{Stream, StreamExt};
use tonic::transport::Server;

use inference_runtime::InferenceJobService;
use toolset_proto::inference_job_client::InferenceJobClient;
use toolset_proto::inference_job_server::InferenceJobServer;
use toolset_proto::run_inference_command::Command;
use toolset_proto::{turn_event, RunInferenceCommand, TurnAssignment};

/// A provider whose `call()` records that it ran, then yields one content delta
/// and a terminal `Done`. The recorded flag is the side-effect the test checks:
/// the pod actually ran the model call, not an echo of the assignment.
struct RecordingProvider {
    called: Arc<AtomicBool>,
}

#[async_trait]
impl LlmProvider for RecordingProvider {
    async fn call(
        &self,
        _messages: &[Message],
        _system: Option<&str>,
        _tools: &[ToolDefinition],
        _params: Option<&serde_json::Map<String, serde_json::Value>>,
        _config: &ProviderConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, String>> + Send>>, String> {
        self.called.store(true, Ordering::SeqCst);
        let events = vec![
            Ok(StreamEvent::ContentDelta {
                text: "served-by-the-pod".into(),
            }),
            Ok(StreamEvent::Done {
                stop_reason: "end_turn".into(),
            }),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }

    fn managed_fields(&self) -> &'static [&'static str] {
        &[]
    }
}

#[tokio::test]
async fn the_pod_serves_run_executes_the_assignment_and_streams_events_back() {
    // Reserve then free an ephemeral port; the client retries over the gap.
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reserve.local_addr().unwrap();
    drop(reserve);

    let called = Arc::new(AtomicBool::new(false));
    let provider = Arc::new(RecordingProvider {
        called: called.clone(),
    });
    let config = ProviderConfig {
        model: "test-model".into(),
        api_key: String::new(),
    };
    let service = InferenceJobService::new(provider, config);
    tokio::spawn(async move {
        Server::builder()
            .add_service(InferenceJobServer::new(service))
            .serve(addr)
            .await
            .unwrap();
    });

    let mut client = loop {
        match InferenceJobClient::connect(format!("http://{addr}")).await {
            Ok(c) => break c,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };

    // The assignment is the first (and here only) message on the outbound half.
    let first = RunInferenceCommand {
        command: Some(Command::Assignment(TurnAssignment {
            system: None,
            tools: vec![],
            messages: vec![],
            conversation_id: "conv-1".to_string(),
        })),
    };

    let response = client
        .run(tokio_stream::once(first))
        .await
        .expect("the pod must serve Run");
    let mut events = response.into_inner();

    let mut completed_text = None;
    while let Some(event) = events.next().await {
        let event = event.expect("an event");
        if let Some(turn_event::Event::Complete(complete)) = event.event {
            let text: String = complete
                .content
                .iter()
                .filter_map(|b| match &b.block {
                    Some(proto_common::content_block::Block::Text(t)) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect();
            completed_text = Some(text);
        }
    }

    assert!(
        called.load(Ordering::SeqCst),
        "the pod must actually run the model provider for the assignment"
    );
    assert_eq!(
        completed_text.as_deref(),
        Some("served-by-the-pod"),
        "the pod must stream a terminal TurnComplete assembled from the model's events"
    );
}
