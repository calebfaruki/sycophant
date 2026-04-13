mod config;

use config::load_config;
use futures::StreamExt;
use tightbeam_proto::convert::{
    proto_message_to_provider, proto_tool_def_to_provider, provider_stop_reason_to_proto,
    stream_event_to_chunk,
};
use tightbeam_proto::tightbeam_controller_client::TightbeamControllerClient;
use tightbeam_proto::GetTurnRequest;
use tightbeam_providers::{LlmProvider, ProviderConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let controller_addr = std::env::var("TIGHTBEAM_CONTROLLER_ADDR")
        .unwrap_or_else(|_| "http://127.0.0.1:9090".into());

    let job_id = std::env::var("TIGHTBEAM_JOB_ID").unwrap_or_else(|_| "local".into());

    let model_name = std::env::var("TIGHTBEAM_MODEL_NAME").unwrap_or_else(|_| "default".into());

    let (format, base_url, config) = load_config()?;
    let llm = format.build(&base_url);

    tracing::info!(
        "connecting to controller at {controller_addr}, job_id={job_id}, model={model_name}"
    );

    let mut client = {
        let mut connected = None;
        for attempt in 1..=10u64 {
            match TightbeamControllerClient::connect(controller_addr.clone()).await {
                Ok(c) => {
                    connected = Some(c);
                    break;
                }
                Err(e) if attempt < 10 => {
                    tracing::warn!(attempt, error = %e, "controller not ready, retrying");
                    tokio::time::sleep(std::time::Duration::from_secs(attempt)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
        connected.unwrap()
    };

    loop {
        let assignment = match client
            .get_turn(GetTurnRequest {
                job_id: job_id.clone(),
                model_name: model_name.clone(),
            })
            .await
        {
            Ok(resp) => resp.into_inner(),
            Err(status) if status.code() == tonic::Code::DeadlineExceeded => {
                tracing::info!("idle timeout, exiting");
                break;
            }
            Err(status) => {
                tracing::error!("GetTurn failed: {status}");
                break;
            }
        };

        if let Err(e) = process_turn(&*llm, &config, &assignment, &mut client, &model_name).await {
            tracing::error!("turn failed: {e}");
        }
    }

    Ok(())
}

async fn process_turn(
    llm: &dyn LlmProvider,
    config: &ProviderConfig,
    assignment: &tightbeam_proto::TurnAssignment,
    client: &mut TightbeamControllerClient<tonic::transport::Channel>,
    model_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let messages: Vec<_> = assignment
        .messages
        .iter()
        .map(proto_message_to_provider)
        .collect();
    let tools: Vec<_> = assignment
        .tools
        .iter()
        .map(proto_tool_def_to_provider)
        .collect();
    let system = assignment.system.as_deref();

    let mut stream = llm
        .call(&messages, system, &tools, config)
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let mut events = Vec::new();
    let mut chunks = Vec::new();

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                chunks.push(stream_event_to_chunk(&event));
                events.push(event);
            }
            Err(e) => {
                chunks.push(tightbeam_proto::TurnResultChunk {
                    chunk: Some(tightbeam_proto::turn_result_chunk::Chunk::Error(
                        tightbeam_proto::TurnError {
                            code: -1,
                            message: e,
                        },
                    )),
                });
                break;
            }
        }
    }

    // Build the final TurnComplete with assembled tool calls and text
    let stop_reason_str = events
        .iter()
        .find_map(|e| match e {
            tightbeam_providers::StreamEvent::Done { stop_reason } => Some(stop_reason.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "end_turn".into());

    let tool_calls = tightbeam_providers::collect_tool_calls(&events);
    let text = tightbeam_providers::collect_text(&events);
    let thinking = tightbeam_providers::collect_thinking(&events);

    let mut final_content: Vec<tightbeam_proto::ContentBlock> = Vec::new();
    if let Some(t) = thinking {
        final_content.push(tightbeam_proto::ContentBlock {
            block: Some(tightbeam_proto::content_block::Block::Thinking(
                tightbeam_proto::ThinkingBlock { text: t },
            )),
        });
    }
    if let Some(t) = text {
        final_content.push(tightbeam_proto::ContentBlock {
            block: Some(tightbeam_proto::content_block::Block::Text(
                tightbeam_proto::TextBlock { text: t },
            )),
        });
    }

    let final_tool_calls: Vec<tightbeam_proto::ToolCall> = tool_calls
        .iter()
        .map(|tc| tightbeam_proto::ToolCall {
            id: tc.id.clone(),
            name: tc.name.clone(),
            input_json: serde_json::to_string(&tc.input).unwrap_or_default(),
        })
        .collect();

    let sr = tightbeam_providers::types::StopReason::from_str_lossy(&stop_reason_str);
    let complete = tightbeam_proto::TurnResultChunk {
        chunk: Some(tightbeam_proto::turn_result_chunk::Chunk::Complete(
            tightbeam_proto::TurnComplete {
                stop_reason: provider_stop_reason_to_proto(&sr),
                content: final_content,
                tool_calls: final_tool_calls,
            },
        )),
    };

    // Replace the Done-generated Complete with the assembled one
    let final_chunks: Vec<_> = chunks
        .into_iter()
        .filter(|c| {
            !matches!(
                c.chunk,
                Some(tightbeam_proto::turn_result_chunk::Chunk::Complete(_)) | None
            )
        })
        .chain(std::iter::once(complete))
        .collect();

    let stream = futures::stream::iter(final_chunks);
    let mut request = tonic::Request::new(stream);
    if let Ok(val) = model_name.parse() {
        request.metadata_mut().insert("x-tightbeam-model", val);
    }
    client.stream_turn_result(request).await?;

    Ok(())
}
