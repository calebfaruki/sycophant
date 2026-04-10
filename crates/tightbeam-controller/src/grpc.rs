use crate::state::{ControllerState, PendingTurn};
use futures::StreamExt;
use std::sync::Arc;
use tightbeam_proto::convert::{
    chunk_to_turn_event, proto_message_to_provider, proto_tool_call_to_provider,
    provider_message_to_proto,
};
use tightbeam_proto::tightbeam_controller_server::TightbeamController;
use tightbeam_proto::{
    channel_inbound, content_block, turn_result_chunk, ChannelInbound, ChannelOutbound,
    GetTurnRequest, InboundMessage, ListModelsRequest, ListModelsResponse, SubscribeRequest,
    TurnAck, TurnAssignment, TurnComplete, TurnEvent, TurnRequest, TurnResultChunk,
};
use tightbeam_providers::types as provider;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

fn assistant_message_from_complete(complete: &TurnComplete) -> provider::Message {
    let text = complete.content.iter().find_map(|b| match &b.block {
        Some(content_block::Block::Text(t)) => Some(t.text.clone()),
        _ => None,
    });

    let tool_calls: Vec<provider::ToolCall> = complete
        .tool_calls
        .iter()
        .map(proto_tool_call_to_provider)
        .collect();

    provider::Message {
        role: "assistant".into(),
        content: text.map(provider::ContentBlock::text_content),
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        is_error: None,
        agent: None,
    }
}

pub struct ControllerService {
    state: Arc<ControllerState>,
}

impl ControllerService {
    pub fn new(state: Arc<ControllerState>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl TightbeamController for ControllerService {
    async fn get_turn(
        &self,
        request: Request<GetTurnRequest>,
    ) -> Result<Response<TurnAssignment>, Status> {
        let req = request.into_inner();
        let model = if req.model_name.is_empty() {
            "default".to_string()
        } else {
            req.model_name
        };

        tracing::info!(model = %model, "get_turn: marking job connected");
        self.state.set_job_connected(&model, true).await;

        tracing::info!(model = %model, "get_turn: waiting for pending turn");
        let pending = self
            .state
            .wait_for_turn(&model)
            .await
            .ok_or_else(|| Status::unavailable("controller shutting down"))?;

        tracing::info!(
            model = %model,
            "get_turn: received assignment with {} messages",
            pending.assignment.messages.len()
        );
        self.state
            .set_active_result_tx(&model, pending.result_tx)
            .await;

        Ok(Response::new(pending.assignment))
    }

    async fn stream_turn_result(
        &self,
        request: Request<Streaming<TurnResultChunk>>,
    ) -> Result<Response<TurnAck>, Status> {
        tracing::info!("stream_turn_result: entry");
        let result_tx = self
            .state
            .take_active_result_tx_any()
            .await
            .ok_or_else(|| Status::failed_precondition("no active turn"))?;

        let mut stream = request.into_inner();
        let mut complete_chunk = None;

        while let Some(chunk) = stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("stream error: {e}")))?
        {
            if matches!(chunk.chunk, Some(turn_result_chunk::Chunk::Complete(_))) {
                complete_chunk = Some(chunk.clone());
            }
            let _ = result_tx.send(chunk).await;
        }

        drop(result_tx);

        if let Some(TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Complete(ref complete)),
            ..
        }) = complete_chunk
        {
            let assistant_msg = assistant_message_from_complete(complete);
            let mut conv = self.state.conversation.write().await;
            let _ = conv.append(assistant_msg);
        }

        Ok(Response::new(TurnAck {}))
    }

    type TurnStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<TurnEvent, Status>> + Send>>;

    async fn turn(
        &self,
        request: Request<TurnRequest>,
    ) -> Result<Response<Self::TurnStream>, Status> {
        tracing::info!("turn: entry");
        let params = request.into_inner();
        let model = params
            .model
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "default".to_string());

        tracing::info!(model = %model, "turn: acquiring conversation write lock");
        let mut conv = self.state.conversation.write().await;
        tracing::info!("turn: lock acquired");

        if let Some(system) = params.system {
            conv.set_system_prompt(system);
        }

        let job_action = self.state.check_job_needed(&model).await;
        if matches!(job_action, crate::state::JobAction::NoModelSpec) {
            return Err(Status::failed_precondition(format!(
                "no TightbeamModel configured for '{model}'"
            )));
        }

        let incoming: Vec<provider::Message> = params
            .messages
            .iter()
            .map(proto_message_to_provider)
            .collect();

        let rollback_len = conv.len();

        conv.append_many(incoming)
            .map_err(|e| Status::internal(format!("conversation append: {e}")))?;

        let history = conv.history_for_provider();
        let system = conv.system_prompt().map(String::from);

        if let crate::state::JobAction::Create(spec) = job_action {
            let client = self.state.kube_client().unwrap();
            let addr = self.state.controller_addr().to_owned();
            let ns = self.state.namespace().to_owned();
            let image = self.state.llm_job_image().to_owned();

            tracing::info!(model = %model, "turn: no LLM Job connected, creating one");
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                crate::job::create_llm_job(client, &model, &spec, &image, &addr, &ns),
            )
            .await
            {
                Ok(Ok(name)) => {
                    tracing::info!(job = %name, "turn: LLM Job created");
                }
                Ok(Err(e)) => {
                    tracing::error!("turn: k8s API rejected Job creation: {e}");
                    conv.truncate(rollback_len);
                    return Err(Status::internal(format!("failed to create LLM Job: {e}")));
                }
                Err(_) => {
                    tracing::error!("turn: k8s API timed out creating Job (10s)");
                    conv.truncate(rollback_len);
                    return Err(Status::internal(
                        "k8s API timed out creating LLM Job".to_string(),
                    ));
                }
            }

            tracing::info!(model = %model, "turn: waiting for Job to connect");
            if !self
                .state
                .wait_for_job_connect(&model, std::time::Duration::from_secs(30))
                .await
            {
                conv.truncate(rollback_len);
                return Err(Status::deadline_exceeded(
                    "LLM Job did not connect within 30s",
                ));
            }
        }

        drop(conv);
        tracing::info!("turn: conversation lock released");

        tracing::info!("turn: building assignment");

        let proto_messages: Vec<_> = history.iter().map(provider_message_to_proto).collect();

        let assignment = TurnAssignment {
            system,
            tools: params.tools,
            messages: proto_messages,
        };

        let (result_tx, result_rx) = mpsc::channel(64);
        let pending = PendingTurn {
            assignment,
            result_tx,
        };

        tracing::info!(model = %model, "turn: enqueueing turn");
        self.state
            .enqueue_turn(&model, pending)
            .await
            .map_err(Status::internal)?;
        tracing::info!("turn: enqueued, returning stream");

        #[allow(clippy::result_large_err)]
        let event_stream = ReceiverStream::new(result_rx)
            .map(|chunk| -> Result<TurnEvent, Status> { Ok(chunk_to_turn_event(chunk)) });

        Ok(Response::new(Box::pin(event_stream)))
    }

    async fn list_models(
        &self,
        _request: Request<ListModelsRequest>,
    ) -> Result<Response<ListModelsResponse>, Status> {
        Ok(Response::new(ListModelsResponse { models: vec![] }))
    }

    type ChannelStreamStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChannelOutbound, Status>> + Send>>;

    async fn channel_stream(
        &self,
        request: Request<Streaming<ChannelInbound>>,
    ) -> Result<Response<Self::ChannelStreamStream>, Status> {
        let mut stream = request.into_inner();
        let state = self.state.clone();

        let (tx, rx) = mpsc::channel(16);

        tokio::spawn(async move {
            while let Ok(Some(inbound)) = stream.message().await {
                match inbound.event {
                    Some(channel_inbound::Event::UserMessage(msg)) => {
                        state.notify_subscriber(InboundMessage {
                            content: msg.content,
                            sender: msg.sender,
                        });
                    }
                    Some(channel_inbound::Event::Register(_)) => {}
                    None => {}
                }
            }
            drop(tx);
        });

        #[allow(clippy::result_large_err)]
        let outbound_stream =
            ReceiverStream::new(rx).map(|msg| -> Result<ChannelOutbound, Status> { Ok(msg) });

        Ok(Response::new(Box::pin(outbound_stream)))
    }

    type SubscribeStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<InboundMessage, Status>> + Send>>;

    async fn subscribe(
        &self,
        _request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let mut rx = self.state.subscribe();

        let (tx, stream_rx) = mpsc::channel(16);

        tokio::spawn(async move {
            while let Ok(msg) = rx.recv().await {
                if tx.send(Ok(msg)).await.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(stream_rx))))
    }
}
