use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use model_provider::{
    assemble_turn_complete, error_turn_event, stream_event_to_turn_event, LlmProvider,
    ProviderConfig, StreamEvent,
};
use shared::keepalive::{Activity, ShutdownWatcher};
use shared::scrub::ScrubSet;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status, Streaming};

use toolset_proto::inference_job_server::InferenceJob;
use toolset_proto::run_inference_command::Command;
use toolset_proto::{
    turn_event, ContentDelta, RunInferenceCommand, ToolUseInput, ToolUseStart, TurnAssignment,
    TurnComplete, TurnError, TurnEvent, TurnWarning,
};

/// Bound on a served call's outbound event channel, matching the harness's
/// inbound bound.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// The gRPC server the harness dials for a model call. It holds no client that
/// dials out to the harness: who may connect is enforced by network policy, so
/// `Run` authenticates structurally. Each `Run` carries one turn — the
/// assignment first, then the pod streams model events back and observes an
/// optional cancel — and runs the model call through the provider built at
/// construction.
pub struct InferenceJobService {
    provider: Arc<dyn LlmProvider>,
    config: Arc<ProviderConfig>,
    scrub: Arc<ScrubSet>,
    activity: Arc<Activity>,
}

impl InferenceJobService {
    pub fn new(provider: Arc<dyn LlmProvider>, config: ProviderConfig) -> Self {
        Self {
            provider,
            config: Arc::new(config),
            scrub: Arc::new(ScrubSet::from_env_var("TOOLSET_SCRUB_SECRETS")),
            activity: Arc::new(Activity::new()),
        }
    }

    /// A handle main waits on to shut the server down once its single call
    /// finishes, or after the idle window with no call in flight.
    pub fn shutdown_watcher(&self) -> ShutdownWatcher {
        ShutdownWatcher::new(self.activity.clone())
    }
}

/// Redact every known secret value from an outbound `TurnEvent` before it
/// leaves the pod. The pod mounts the provider credential, so a provider error
/// that echoes the key, or a content delta the model was injected into emitting
/// its own key, must not put the credential plaintext on the wire to the
/// harness. Mirrors the tool job's per-frame scrub of its `ToolResultFrame`s.
fn scrub_turn_event(event: TurnEvent, scrub: &ScrubSet) -> TurnEvent {
    let scrubbed = event.event.map(|e| match e {
        turn_event::Event::ContentDelta(d) => turn_event::Event::ContentDelta(ContentDelta {
            text: scrub.apply(&d.text),
        }),
        turn_event::Event::ToolUseStart(s) => turn_event::Event::ToolUseStart(ToolUseStart {
            id: scrub.apply(&s.id),
            name: scrub.apply(&s.name),
        }),
        turn_event::Event::ToolUseInput(i) => turn_event::Event::ToolUseInput(ToolUseInput {
            partial_json: scrub.apply(&i.partial_json),
        }),
        turn_event::Event::Complete(c) => {
            turn_event::Event::Complete(scrub_turn_complete(c, scrub))
        }
        turn_event::Event::Error(e) => turn_event::Event::Error(TurnError {
            code: e.code,
            message: scrub.apply(&e.message),
        }),
        turn_event::Event::Warning(w) => turn_event::Event::Warning(TurnWarning {
            field: scrub.apply(&w.field),
            reason: scrub.apply(&w.reason),
        }),
    });
    TurnEvent { event: scrubbed }
}

/// Redact every text block and tool-call field of an authoritative
/// `TurnComplete`, so the assembled result the harness reads carries no secret
/// the model echoed into its content or a tool-call argument.
fn scrub_turn_complete(mut complete: TurnComplete, scrub: &ScrubSet) -> TurnComplete {
    for block in &mut complete.content {
        match block.block.as_mut() {
            Some(proto_common::content_block::Block::Text(t)) => t.text = scrub.apply(&t.text),
            Some(proto_common::content_block::Block::Thinking(t)) => t.text = scrub.apply(&t.text),
            _ => {}
        }
    }
    for call in &mut complete.tool_calls {
        call.id = scrub.apply(&call.id);
        call.name = scrub.apply(&call.name);
        call.input_json = scrub.apply(&call.input_json);
    }
    complete
}

/// Open the provider call for one assignment and drain it onto `tx` as
/// `TurnEvent` frames: forward each delta, assemble the authoritative
/// `TurnComplete` on the provider's terminal `Done`, and surface an open-time or
/// mid-stream error as a single `TurnError`. A fired `cancel` (a cancel pushed on
/// the same held stream) drops the provider stream and ends without a terminal.
async fn run_model_call(
    provider: Arc<dyn LlmProvider>,
    config: Arc<ProviderConfig>,
    scrub: Arc<ScrubSet>,
    assignment: TurnAssignment,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<TurnEvent>,
) {
    let mut stream = match provider
        .call(
            &assignment.messages,
            assignment.system.as_deref(),
            &assignment.tools,
            None,
            &config,
        )
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            let _ = tx
                .send(scrub_turn_event(
                    error_turn_event(format!("model call failed: {e}")),
                    &scrub,
                ))
                .await;
            return;
        }
    };

    let mut events: Vec<StreamEvent> = Vec::new();
    loop {
        tokio::select! {
            biased;
            // Cancel wins: return WITHOUT assembling or sending a Complete.
            // Returning drops `stream`, which abandons the provider call.
            _ = cancel.cancelled() => return,
            maybe = stream.next() => match maybe {
                Some(Ok(event)) => {
                    events.push(event.clone());
                    if let StreamEvent::Done { stop_reason } = &event {
                        let _ = tx
                            .send(scrub_turn_event(
                                assemble_turn_complete(&events, stop_reason),
                                &scrub,
                            ))
                            .await;
                        return;
                    }
                    if let Some(frame) = stream_event_to_turn_event(&event) {
                        if tx.send(scrub_turn_event(frame, &scrub)).await.is_err() {
                            return;
                        }
                    }
                }
                Some(Err(e)) => {
                    let _ = tx
                        .send(scrub_turn_event(
                            error_turn_event(format!("model stream error: {e}")),
                            &scrub,
                        ))
                        .await;
                    return;
                }
                None => {
                    let _ = tx
                        .send(scrub_turn_event(
                            error_turn_event(
                                "model stream ended without completion".to_string(),
                            ),
                            &scrub,
                        ))
                        .await;
                    return;
                }
            }
        }
    }
}

type EventStream = Pin<Box<dyn Stream<Item = Result<TurnEvent, Status>> + Send>>;

#[tonic::async_trait]
impl InferenceJob for InferenceJobService {
    type RunStream = EventStream;

    async fn run(
        &self,
        request: Request<Streaming<RunInferenceCommand>>,
    ) -> Result<Response<Self::RunStream>, Status> {
        let mut inbound = request.into_inner();

        // The assignment is the first message on the stream. A pod that called
        // the model before reading it would run the wrong turn.
        let first = inbound
            .next()
            .await
            .ok_or_else(|| {
                Status::invalid_argument("the harness must send an assignment before any command")
            })?
            .map_err(|e| Status::internal(format!("assignment stream error: {e}")))?;
        let assignment = match first.command {
            Some(Command::Assignment(a)) => a,
            _ => {
                return Err(Status::invalid_argument(
                    "the first RunInferenceCommand must be the model-call assignment",
                ))
            }
        };

        // A cancel on the request half fires the local token, so the in-flight
        // model call is abandoned on the same held connection — no pod-initiated
        // call.
        let cancel = CancellationToken::new();
        let watch = cancel.clone();
        tokio::spawn(async move {
            while let Some(Ok(cmd)) = inbound.next().await {
                if matches!(cmd.command, Some(Command::Cancel(_))) {
                    watch.cancel();
                    break;
                }
            }
        });

        self.activity.in_flight.fetch_add(1, Ordering::SeqCst);
        self.activity.touch().await;

        let (tx, rx) = mpsc::channel::<TurnEvent>(EVENT_CHANNEL_CAPACITY);
        let provider = self.provider.clone();
        let config = self.config.clone();
        let scrub = self.scrub.clone();
        let activity = self.activity.clone();
        tokio::spawn(async move {
            run_model_call(provider, config, scrub, assignment, &cancel, &tx).await;
            activity.completed.fetch_add(1, Ordering::SeqCst);
            activity.in_flight.fetch_sub(1, Ordering::SeqCst);
            activity.touch().await;
        });

        let stream = ReceiverStream::new(rx).map(Ok::<TurnEvent, Status>);
        Ok(Response::new(Box::pin(stream)))
    }
}
