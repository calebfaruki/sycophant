//! The inference-runtime pod mounts the provider credential, so it must redact
//! that secret from every `TurnEvent` it streams back to the (semi-adversarial)
//! harness — the same per-frame scrub the tool job applies to its result
//! frames. This drives a provider that emits its own key in a content delta
//! (the prompt-injection shape) and asserts the streamed frames carry the
//! redaction tag, not the plaintext key.
//!
//! Materiality: with the scrub unwired, the model's content delta and the
//! assembled `TurnComplete` reach the harness with the raw credential in them,
//! reding both the "no plaintext" and the "redaction tag present" assertions.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use model_provider::{LlmProvider, ProviderConfig, StreamEvent};
use proto_common::{Message, ToolDefinition};
use serial_test::serial;
use tokio_stream::{Stream, StreamExt};
use tonic::transport::Server;

use inference_runtime::InferenceJobService;
use toolset_proto::inference_job_client::InferenceJobClient;
use toolset_proto::inference_job_server::InferenceJobServer;
use toolset_proto::run_inference_command::Command;
use toolset_proto::{turn_event, RunInferenceCommand, TurnAssignment};

const SECRET: &str = "sk-provider-live-key-abc123";

/// A provider that emits the mounted credential inside a content delta, then a
/// terminal `Done`. Stands in for a prompt-injected model made to echo its own
/// key, or a provider error carrying it.
struct LeakingProvider;

#[async_trait]
impl LlmProvider for LeakingProvider {
    async fn call(
        &self,
        _messages: &[Message],
        _system: Option<&str>,
        _tools: &[ToolDefinition],
        _params: Option<&serde_json::Map<String, serde_json::Value>>,
        _config: &ProviderConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, String>> + Send>>, String> {
        let events = vec![
            Ok(StreamEvent::ContentDelta {
                text: format!("here is my key: {SECRET}"),
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
#[serial]
async fn the_pod_scrubs_the_provider_secret_from_every_outbound_frame() {
    // Arm the service's scrub set with the provider credential BEFORE the
    // service is constructed — it reads the registry at construction, exactly
    // as the pod does. Both env vars are removed once the set is built.
    std::env::set_var("INFERENCE_SCRUB_TEST_SECRET", SECRET);
    std::env::set_var(
        "TOOLSET_SCRUB_SECRETS",
        r#"[{"name":"provider","env":"INFERENCE_SCRUB_TEST_SECRET"}]"#,
    );

    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reserve.local_addr().unwrap();
    drop(reserve);

    let config = ProviderConfig {
        model: "test-model".into(),
        api_key: SECRET.into(),
    };
    let service = InferenceJobService::new(Arc::new(LeakingProvider), config);

    std::env::remove_var("TOOLSET_SCRUB_SECRETS");
    std::env::remove_var("INFERENCE_SCRUB_TEST_SECRET");

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

    let mut delta_text = String::new();
    let mut complete_text = String::new();
    while let Some(event) = events.next().await {
        let event = event.expect("an event");
        match event.event {
            Some(turn_event::Event::ContentDelta(d)) => delta_text.push_str(&d.text),
            Some(turn_event::Event::Complete(c)) => {
                complete_text = c
                    .content
                    .iter()
                    .filter_map(|b| match &b.block {
                        Some(proto_common::content_block::Block::Text(t)) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();
            }
            _ => {}
        }
    }

    assert!(
        !delta_text.contains(SECRET),
        "the streamed content delta must be scrubbed before it leaves the pod; leaked in {delta_text:?}"
    );
    assert!(
        !complete_text.contains(SECRET),
        "the assembled TurnComplete content must be scrubbed; leaked in {complete_text:?}"
    );
    let joined = format!("{delta_text}|{complete_text}");
    assert!(
        joined.contains("[REDACTED:provider]"),
        "scrubbed frames carry the redaction tag (proves scrub ran, not that frames were dropped), got {joined:?}"
    );
}
