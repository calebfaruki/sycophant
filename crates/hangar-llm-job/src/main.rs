mod config;
mod scrub_chunk;
mod scrub_writer;

use config::load_config;
use futures::{SinkExt, StreamExt};
use hangar_proto::convert::{
    proto_message_to_provider, proto_tool_def_to_provider, provider_stop_reason_to_proto,
    stream_event_to_chunk,
};
use hangar_proto::hangar_controller_client::HangarControllerClient;
use hangar_proto::{AwaitTurnCancelRequest, GetTurnRequest};
use hangar_providers::{LlmProvider, ProviderConfig};
use scrub_chunk::scrub_chunk;
use scrub_writer::ScrubMakeWriter;
use shared::auth::SaTokenInterceptor;
use shared::scrub::ScrubSet;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load scrub set FIRST so the tracing subscriber gets a writer that
    // can redact registered secrets even on the first log line.
    let scrub_set = Arc::new(ScrubSet::from_env_var("HANGAR_SCRUB_SECRETS"));
    tracing_subscriber::fmt()
        .with_writer(ScrubMakeWriter::new(scrub_set.clone()))
        .init();

    let controller_addr =
        std::env::var("HANGAR_CONTROLLER_ADDR").unwrap_or_else(|_| "http://127.0.0.1:9090".into());

    let model_name = std::env::var("HANGAR_MODEL_NAME").map_err(|_| {
        "HANGAR_MODEL_NAME env var is required (set by the controller when spawning the LLM Job)"
    })?;

    let (format, base_url, config) = load_config()?;
    let llm = format.build(&base_url);

    tracing::info!("connecting to controller at {controller_addr}, model={model_name}");

    // Establish the raw channel, then wrap with SaTokenInterceptor so
    // every outgoing RPC (GetTurn / StreamTurnResult) carries the pod's
    // ServiceAccount token as a Bearer header. The controller's
    // internal listener verifies the token via TokenReview and binds
    // the caller to `sa-<workspace>` — this is the LLM Job's identity.
    let channel =
        shared::grpc_client::connect_with_keepalive(&controller_addr, "hangar-controller").await?;
    let mut client =
        HangarControllerClient::with_interceptor(channel, SaTokenInterceptor::default_path());

    loop {
        let assignment = match client
            .get_turn(GetTurnRequest {
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

        // Open the cancel channel for this turn: a watcher long-polls
        // AwaitTurnCancel keyed by the turn's conversation_id and fires the
        // local token when a cancel arrives, so drain_stream can abandon the
        // in-flight provider call. Aborted once the turn returns.
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_watcher = {
            let mut cancel_client = client.clone();
            let watch_token = cancel.clone();
            let conversation_id = assignment.conversation_id.clone();
            tokio::spawn(async move {
                if cancel_client
                    .await_turn_cancel(AwaitTurnCancelRequest {
                        conversation_id: conversation_id.clone(),
                    })
                    .await
                    .is_ok()
                {
                    tracing::info!(
                        conversation_id,
                        "cancel signal received; abandoning in-flight model call"
                    );
                    watch_token.cancel();
                }
            })
        };

        // Backstop: guarantee the worker ALWAYS returns to get_turn even if a
        // turn wedges (a future blocking await, or a provider that opens then
        // streams only heartbeats forever and never a Complete). The controller
        // choke point handles the common consumer-stall case far sooner; this
        // is the last-resort seatbelt sized above any legitimate turn. The
        // cancel token is the responsive path; this stays a backstop.
        match tokio::time::timeout(
            std::time::Duration::from_secs(PROCESS_TURN_BACKSTOP_SECS),
            process_turn(
                &*llm,
                &config,
                &assignment,
                &mut client,
                &model_name,
                &scrub_set,
                &cancel,
            ),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("turn failed: {e}"),
            Err(_) => tracing::error!(
                "turn exceeded {PROCESS_TURN_BACKSTOP_SECS}s backstop; abandoning to free the worker"
            ),
        }

        cancel_watcher.abort();
    }

    Ok(())
}

/// Wall-clock seatbelt on an entire turn: reqwest's own `.timeout(600s)`
/// already bounds "provider streams forever", so 660 = 600 + 60s slack means
/// this only fires for a wedge OUTSIDE the HTTP stream (a deadlock in the
/// producer/`join!` logic itself) — the class of bug this seatbelt exists to
/// catch on the sole keepalive worker. The controller's per-chunk forward
/// budget handles a stalled consumer long before this.
const PROCESS_TURN_BACKSTOP_SECS: u64 = 660;

/// Heartbeat cadence while the provider is generating. Must stay well
/// under the transponder's idle-gap so a slow-but-alive turn never trips
/// it. An empty `ContentDelta` carries the heartbeat — the transponder
/// treats it as a non-terminal event (resets the gap) and skips it.
const HEARTBEAT_SECS: u64 = 10;

/// Upper bound on time-to-response-headers for the provider call. The
/// heartbeat only starts once the stream is open, so a provider that accepts
/// the connection but never returns headers would otherwise stall silently
/// until the 600s request timeout — long past the transponder's idle gap.
/// Bounding it below that gap surfaces a `TurnError` instead of a silent wedge.
const OPEN_TIMEOUT_SECS: u64 = 30;

/// Build a scrubbed `TurnError` chunk and send it on the result channel. Every
/// producer error path — an open-time failure, an open timeout, or a mid-stream
/// provider error — surfaces the same way.
async fn send_error_chunk(
    tx: &mut futures::channel::mpsc::Sender<hangar_proto::TurnResultChunk>,
    scrub_set: &ScrubSet,
    message: String,
) {
    let mut chunk = hangar_proto::TurnResultChunk {
        chunk: Some(hangar_proto::turn_result_chunk::Chunk::Error(
            hangar_proto::TurnError { code: -1, message },
        )),
    };
    scrub_chunk(&mut chunk, scrub_set);
    let _ = tx.send(chunk).await;
}

/// Open the provider stream, bounded by `OPEN_TIMEOUT_SECS`. Any open-time
/// failure — a provider error (e.g. an HTTP 400) or a headers timeout — is
/// reported as a single `TurnError` chunk on the result channel and returns
/// `None`, so the producer just returns. This is what makes an open-time
/// failure reach the controller (and thus the client) as FAILED instead of a
/// silent early return that strands the turn. The timeout matters because the
/// heartbeat only starts once the stream is open: a provider that accepts the
/// connection but never returns headers would otherwise stall silently past
/// the transponder's idle gap until the 600s request timeout.
#[allow(clippy::too_many_arguments)]
async fn open_or_timeout(
    llm: &dyn LlmProvider,
    messages: &[hangar_providers::Message],
    system: Option<&str>,
    tools: &[hangar_providers::ToolDefinition],
    params: Option<&serde_json::Map<String, serde_json::Value>>,
    config: &ProviderConfig,
    scrub_set: &ScrubSet,
    tx: &mut futures::channel::mpsc::Sender<hangar_proto::TurnResultChunk>,
) -> Option<
    std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<hangar_providers::StreamEvent, String>> + Send>,
    >,
> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(OPEN_TIMEOUT_SECS),
        llm.call(messages, system, tools, params, config),
    )
    .await
    {
        Ok(Ok(stream)) => Some(stream),
        Ok(Err(e)) => {
            send_error_chunk(tx, scrub_set, e).await;
            None
        }
        Err(_) => {
            send_error_chunk(
                tx,
                scrub_set,
                format!("provider did not return response headers within {OPEN_TIMEOUT_SECS}s"),
            )
            .await;
            None
        }
    }
}

/// Drain an already-open provider stream to the result channel: forward each
/// event as a chunk (dropping the provider's own Complete), heartbeat on
/// silence, and — on clean end-of-stream — send one assembled Complete. A
/// mid-stream provider error is reported as a single `TurnError` chunk and ends
/// the drain (never falls through to the Complete), mirroring `open_or_timeout`
/// so the client sees FAILED instead of a silent hang. Extracted from
/// `process_turn` to give the mid-stream error path a testable seam.
async fn drain_stream(
    mut stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<hangar_providers::StreamEvent, String>> + Send>,
    >,
    tx: &mut futures::channel::mpsc::Sender<hangar_proto::TurnResultChunk>,
    scrub_set: &ScrubSet,
    token: &tokio_util::sync::CancellationToken,
) {
    let mut events = Vec::new();
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(HEARTBEAT_SECS));
    heartbeat.tick().await; // drop the immediate first tick
    loop {
        tokio::select! {
            biased;
            // Cancel wins: return early WITHOUT assembling or sending a
            // Complete. Returning drops `stream` (the boxed provider SSE
            // stream), which drops the reqwest body and abandons the call.
            _ = token.cancelled() => {
                tracing::info!("model call abandoned: dropping provider stream");
                return;
            }
            maybe = stream.next() => match maybe {
                Some(Ok(event)) => {
                    let mut chunk = stream_event_to_chunk(&event);
                    // Drop the provider's Complete/empty chunk; the
                    // assembled Complete below is authoritative.
                    let drop_chunk = matches!(
                        chunk.chunk,
                        Some(hangar_proto::turn_result_chunk::Chunk::Complete(_)) | None
                    );
                    if !drop_chunk {
                        scrub_chunk(&mut chunk, scrub_set);
                        let _ = tx.send(chunk).await;
                    }
                    events.push(event);
                }
                Some(Err(e)) => {
                    send_error_chunk(tx, scrub_set, e).await;
                    return;
                }
                None => break,
            },
            _ = heartbeat.tick() => {
                let _ = tx
                    .send(hangar_proto::TurnResultChunk {
                        chunk: Some(hangar_proto::turn_result_chunk::Chunk::ContentDelta(
                            hangar_proto::ContentDelta { text: String::new() },
                        )),
                    })
                    .await;
            }
        }
    }

    // Assemble the authoritative Complete from the collected events.
    let stop_reason_str = events
        .iter()
        .find_map(|e| match e {
            hangar_providers::StreamEvent::Done { stop_reason } => Some(stop_reason.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "end_turn".into());
    let tool_calls = hangar_providers::collect_tool_calls(&events);
    let text = hangar_providers::collect_text(&events);
    let thinking = hangar_providers::collect_thinking(&events);

    let mut final_content: Vec<hangar_proto::ContentBlock> = Vec::new();
    if let Some(t) = thinking {
        final_content.push(hangar_proto::ContentBlock {
            block: Some(hangar_proto::content_block::Block::Thinking(
                hangar_proto::ThinkingBlock { text: t },
            )),
        });
    }
    if let Some(t) = text {
        final_content.push(hangar_proto::ContentBlock {
            block: Some(hangar_proto::content_block::Block::Text(
                hangar_proto::TextBlock { text: t },
            )),
        });
    }
    let final_tool_calls: Vec<hangar_proto::ToolCall> = tool_calls
        .iter()
        .map(|tc| hangar_proto::ToolCall {
            id: tc.id.clone(),
            name: tc.name.clone(),
            input_json: serde_json::to_string(&tc.input).unwrap_or_default(),
        })
        .collect();
    let sr = hangar_providers::types::StopReason::from_str_lossy(&stop_reason_str);
    let mut complete = hangar_proto::TurnResultChunk {
        chunk: Some(hangar_proto::turn_result_chunk::Chunk::Complete(
            hangar_proto::TurnComplete {
                stop_reason: provider_stop_reason_to_proto(&sr),
                content: final_content,
                tool_calls: final_tool_calls,
            },
        )),
    };
    scrub_chunk(&mut complete, scrub_set);
    let _ = tx.send(complete).await;
}

async fn process_turn(
    llm: &dyn LlmProvider,
    config: &ProviderConfig,
    assignment: &hangar_proto::TurnAssignment,
    client: &mut HangarControllerClient<
        tonic::service::interceptor::InterceptedService<
            tonic::transport::Channel,
            SaTokenInterceptor,
        >,
    >,
    model_name: &str,
    scrub_set: &ScrubSet,
    token: &tokio_util::sync::CancellationToken,
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
    let params = assignment
        .params_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(s).ok());

    // Stream chunks to the controller AS the provider produces them, with
    // a heartbeat on silence. The live stream (vs the old buffer-then-send)
    // is what lets the transponder's idle-gap measure real wedging instead
    // of a slow-but-alive generation. The provider's own Done-generated
    // Complete is dropped; the producer sends an assembled Complete (with
    // collected tool calls + text) as the final chunk. `futures` mpsc gives
    // a Receiver that is itself the request Stream. The provider call runs
    // INSIDE the producer (below) so an open-time failure (e.g. a 400)
    // surfaces as a TurnError chunk on the same path as a mid-stream error
    // — never a silent early return that strands the controller's turn.
    let (mut tx, rx) = futures::channel::mpsc::channel::<hangar_proto::TurnResultChunk>(64);

    // Owned clone for the `async move` producer to hand to drain_stream.
    let drain_token = token.clone();

    // `async move` so the producer OWNS `tx`: when it finishes, `tx` drops and
    // `rx` (the controller's request stream) hits EOF, which is what lets the
    // controller close the turn and return TurnAck. A borrowing `async {}` here
    // keeps `tx` alive until process_turn's scope ends — after the join! — so
    // `rx` never ends, the controller loops forever, and the worker wedges.
    let producer = async move {
        // Open the provider stream here, inside the producer, so an open-time
        // error (e.g. an HTTP 400) becomes a TurnError chunk on the same path
        // as a mid-stream error instead of a silent early return that strands
        // the controller's active turn (the client hangs on "Working…").
        let Some(stream) = open_or_timeout(
            llm,
            &messages,
            system,
            &tools,
            params.as_ref(),
            config,
            scrub_set,
            &mut tx,
        )
        .await
        else {
            return;
        };
        drain_stream(stream, &mut tx, scrub_set, &drain_token).await;
        // producer owns `tx` (async move); it drops here → rx (request stream)
        // ends → the controller closes the turn and returns TurnAck.
    };

    let sender = async {
        let mut request = tonic::Request::new(rx);
        if let Ok(val) = model_name.parse() {
            request.metadata_mut().insert("x-hangar-model", val);
        }
        client.stream_turn_result(request).await
    };

    let (_, send_result) = tokio::join!(producer, sender);
    send_result?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hangar_providers::{Message, StreamEvent, ToolDefinition};

    /// Provider whose `call()` fails at open time — mirrors a provider 400 /
    /// credit error returned before any stream is produced.
    struct FailOpenProvider(String);

    #[async_trait::async_trait]
    impl LlmProvider for FailOpenProvider {
        async fn call(
            &self,
            _messages: &[Message],
            _system: Option<&str>,
            _tools: &[ToolDefinition],
            _params: Option<&serde_json::Map<String, serde_json::Value>>,
            _config: &ProviderConfig,
        ) -> Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = Result<StreamEvent, String>> + Send>>,
            String,
        > {
            Err(self.0.clone())
        }

        fn managed_fields(&self) -> &'static [&'static str] {
            &[]
        }
    }

    #[tokio::test]
    async fn open_time_error_emits_one_turn_error_chunk() {
        // The fix: an open-time provider failure must be reported as a single
        // TurnError chunk on the result channel (so the controller broadcasts
        // FAILED), and the helper returns None so the producer just returns.
        // Mutant: revert to an early `?` → no chunk is ever sent, this is red.
        let llm = FailOpenProvider("API error 401: unauthorized".into());
        let config = ProviderConfig {
            model: "m".into(),
            api_key: String::new(),
        };
        let scrub = ScrubSet::from_env_var("__HANGAR_TEST_NO_SCRUB__");
        let (mut tx, mut rx) = futures::channel::mpsc::channel::<hangar_proto::TurnResultChunk>(4);

        // FailOpenProvider errors immediately, so this hits the provider-error
        // arm well before the timeout is ever armed.
        let opened = open_or_timeout(&llm, &[], None, &[], None, &config, &scrub, &mut tx).await;
        assert!(
            opened.is_none(),
            "a failed open returns None so the producer just returns"
        );
        drop(tx);

        let chunk = rx.next().await.expect("an error chunk must be sent");
        match chunk.chunk {
            Some(hangar_proto::turn_result_chunk::Chunk::Error(e)) => {
                assert_eq!(e.code, -1);
                assert!(e.message.contains("401"), "carries the provider error text");
            }
            other => panic!("expected a TurnError chunk, got {other:?}"),
        }
        assert!(
            rx.next().await.is_none(),
            "exactly one chunk, then the channel closes"
        );
    }

    /// Provider whose `call()` never resolves — mirrors a provider that accepts
    /// the connection but never returns response headers.
    struct NeverReturnsProvider;

    #[async_trait::async_trait]
    impl LlmProvider for NeverReturnsProvider {
        async fn call(
            &self,
            _messages: &[Message],
            _system: Option<&str>,
            _tools: &[ToolDefinition],
            _params: Option<&serde_json::Map<String, serde_json::Value>>,
            _config: &ProviderConfig,
        ) -> Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = Result<StreamEvent, String>> + Send>>,
            String,
        > {
            futures::future::pending().await
        }

        fn managed_fields(&self) -> &'static [&'static str] {
            &[]
        }
    }

    // start_paused lets tokio auto-advance the clock past OPEN_TIMEOUT_SECS the
    // moment the runtime is otherwise idle, so this test is instant, not 30s.
    #[tokio::test(start_paused = true)]
    async fn open_that_never_returns_headers_times_out_to_error() {
        // The fix: a provider that never returns headers must be bounded by
        // OPEN_TIMEOUT_SECS and reported as a TurnError (so the turn ends
        // instead of stranding the transponder for the full 600s request
        // timeout). Mutant: drop the tokio::time::timeout wrapper in
        // open_or_timeout → this hangs forever (test times out red).
        let llm = NeverReturnsProvider;
        let config = ProviderConfig {
            model: "m".into(),
            api_key: String::new(),
        };
        let scrub = ScrubSet::from_env_var("__HANGAR_TEST_NO_SCRUB__");
        let (mut tx, mut rx) = futures::channel::mpsc::channel::<hangar_proto::TurnResultChunk>(4);

        let opened = open_or_timeout(&llm, &[], None, &[], None, &config, &scrub, &mut tx).await;
        assert!(
            opened.is_none(),
            "a provider that never returns headers times out to None"
        );
        drop(tx);

        let chunk = rx.next().await.expect("a timeout error chunk must be sent");
        match chunk.chunk {
            Some(hangar_proto::turn_result_chunk::Chunk::Error(e)) => {
                assert_eq!(e.code, -1);
                assert!(
                    e.message.contains("headers"),
                    "carries the header-timeout reason"
                );
            }
            other => panic!("expected a TurnError chunk, got {other:?}"),
        }
    }

    /// Provider whose stream yields one delta then errors — mirrors a provider
    /// connection that drops mid-generation, after response headers.
    struct StreamThenError(String);

    #[async_trait::async_trait]
    impl LlmProvider for StreamThenError {
        async fn call(
            &self,
            _messages: &[Message],
            _system: Option<&str>,
            _tools: &[ToolDefinition],
            _params: Option<&serde_json::Map<String, serde_json::Value>>,
            _config: &ProviderConfig,
        ) -> Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = Result<StreamEvent, String>> + Send>>,
            String,
        > {
            let events = vec![
                Ok(StreamEvent::ContentDelta {
                    text: "partial".into(),
                }),
                Err(self.0.clone()),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }

        fn managed_fields(&self) -> &'static [&'static str] {
            &[]
        }
    }

    #[tokio::test]
    async fn mid_stream_provider_error_emits_one_turn_error_chunk_then_returns() {
        // A provider stream that errors AFTER headers (a dropped connection
        // mid-generation) must surface as a single TurnError chunk so the
        // controller broadcasts FAILED — never fall through to the assembled
        // Complete, which would strand the client on "Working…". Mutant: change
        // the Some(Err(e)) arm in drain_stream to `break` (swallow, fall
        // through to Complete) → the last chunk is a Complete, no Error, red.
        let llm = StreamThenError("provider 500: connection reset".into());
        let config = ProviderConfig {
            model: "m".into(),
            api_key: String::new(),
        };
        let scrub = ScrubSet::from_env_var("__HANGAR_TEST_NO_SCRUB__");
        let stream = llm
            .call(&[], None, &[], None, &config)
            .await
            .expect("stream opens fine — the error is mid-stream");
        let (mut tx, mut rx) = futures::channel::mpsc::channel::<hangar_proto::TurnResultChunk>(8);

        // An un-fired token: the mid-stream-error path under test is unchanged
        // by the cancel plumbing.
        drain_stream(
            stream,
            &mut tx,
            &scrub,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
        drop(tx);

        let mut chunks = Vec::new();
        while let Some(c) = rx.next().await {
            chunks.push(c);
        }
        match chunks
            .last()
            .expect("at least the error chunk")
            .chunk
            .clone()
        {
            Some(hangar_proto::turn_result_chunk::Chunk::Error(e)) => {
                assert_eq!(e.code, -1);
                assert!(e.message.contains("500"), "carries the provider error text");
            }
            other => panic!("expected the last chunk to be a TurnError, got {other:?}"),
        }
        assert!(
            !chunks.iter().any(|c| matches!(
                c.chunk,
                Some(hangar_proto::turn_result_chunk::Chunk::Complete(_))
            )),
            "a mid-stream error must not be followed by an assembled Complete"
        );
    }

    // The provider stream is abandonable by dropping it. `drain_stream` gains a
    // per-turn cancel token (fired by the AwaitTurnCancel long-poll returning);
    // when it fires, drain_stream must return early — dropping `stream` (and
    // with it the reqwest body) — WITHOUT draining the stream or assembling a
    // Complete.
    #[tokio::test]
    async fn cancel_abandons_provider_stream_without_completing() {
        // A provider stream that never ends: it yields deltas forever, so only
        // a cancel can end the drain. A drained/looping consume hangs.
        let scrub = ScrubSet::from_env_var("__HANGAR_TEST_NO_SCRUB__");
        let stream: std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<StreamEvent, String>> + Send>,
        > = Box::pin(futures::stream::repeat_with(|| {
            Ok(StreamEvent::ContentDelta {
                text: "more".into(),
            })
        }));
        let (mut tx, mut rx) = futures::channel::mpsc::channel::<hangar_proto::TurnResultChunk>(8);

        let token = tokio_util::sync::CancellationToken::new();
        token.cancel(); // fired before the drain begins

        // Materiality: drop the biased `_ = token.cancelled() => return` arm
        // added to drain_stream's select -> the endless stream is drained
        // forever -> this 2s timeout reds.
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            drain_stream(stream, &mut tx, &scrub, &token),
        )
        .await
        .expect("a fired cancel must make drain_stream return promptly, not drain forever");
        drop(tx);

        let mut chunks = Vec::new();
        while let Some(c) = rx.next().await {
            chunks.push(c);
        }
        // Materiality: return early but still send the assembled Complete ->
        // this reds. Abandon means no terminal Complete for a cancelled call.
        assert!(
            !chunks.iter().any(|c| matches!(
                c.chunk,
                Some(hangar_proto::turn_result_chunk::Chunk::Complete(_))
            )),
            "an abandoned stream must not assemble a Complete"
        );
    }
}
