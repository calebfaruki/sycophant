use crate::state::{ControllerState, PendingTurn};
use futures::StreamExt;
use std::sync::Arc;
use sycophant_auth::{extract_bearer_token, TokenVerifier};
use tightbeam_proto::convert::{
    chunk_to_turn_event, proto_message_to_provider, proto_tool_call_to_provider,
    provider_message_to_proto,
};
use tightbeam_proto::tightbeam_controller_server::TightbeamController;
use tightbeam_proto::{
    channel_inbound, channel_outbound, content_block, turn_result_chunk, ChannelInbound,
    ChannelOutbound, ChannelSend, GetTurnRequest, InboundMessage, ListModelsRequest,
    ListModelsResponse, SubscribeRequest, TurnAck, TurnAssignment, TurnComplete, TurnEvent,
    TurnRequest, TurnResultChunk,
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
    verifier: Option<Arc<dyn TokenVerifier>>,
}

impl ControllerService {
    pub fn new(state: Arc<ControllerState>, verifier: Option<Arc<dyn TokenVerifier>>) -> Self {
        Self { state, verifier }
    }

    async fn verify_workspace<T>(&self, request: &Request<T>) -> Result<String, Status> {
        match &self.verifier {
            Some(v) => {
                let token = extract_bearer_token(request)?;
                v.verify_token(token).await
            }
            None => Ok("default".to_string()),
        }
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
            .set_active_turn(
                &model,
                pending.workspace,
                pending.reply_channel,
                pending.result_tx,
            )
            .await;

        Ok(Response::new(pending.assignment))
    }

    async fn stream_turn_result(
        &self,
        request: Request<Streaming<TurnResultChunk>>,
    ) -> Result<Response<TurnAck>, Status> {
        tracing::info!("stream_turn_result: entry");

        let model = request
            .metadata()
            .get("x-tightbeam-model")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| Status::invalid_argument("missing x-tightbeam-model metadata header"))?;

        let active = self
            .state
            .take_active_turn(&model)
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
            let _ = active.result_tx.send(chunk).await;
        }

        drop(active.result_tx);

        if let Some(TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Complete(ref complete)),
            ..
        }) = complete_chunk
        {
            let assistant_msg = assistant_message_from_complete(complete);
            let ws = self.state.get_or_create_workspace(&active.workspace).await;
            let mut conv = ws.conversation.write().await;
            let _ = conv.append(assistant_msg);

            if complete.stop_reason == 1 {
                if let Some(ref channel_key) = active.reply_channel {
                    let outbound = ChannelOutbound {
                        command: Some(channel_outbound::Command::SendMessage(ChannelSend {
                            content: complete.content.clone(),
                        })),
                    };
                    self.state.send_to_channel(channel_key, outbound).await;
                }
            }
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
        let workspace = self.verify_workspace(&request).await?;
        let params = request.into_inner();
        let model = params
            .model
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "default".to_string());

        let ws = self.state.get_or_create_workspace(&workspace).await;

        tracing::info!(model = %model, workspace = %workspace, "turn: acquiring conversation write lock");
        let mut conv = ws.conversation.write().await;
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
                crate::job::create_llm_job(client, &model, &spec, &image, &addr, &ns, &workspace),
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
            workspace,
            reply_channel: None,
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

        let first = stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("stream error: {e}")))?
            .ok_or_else(|| Status::invalid_argument("empty stream"))?;

        let (channel_key, workspace) = match first.event {
            Some(channel_inbound::Event::Register(reg)) => {
                let workspace = reg.workspace.unwrap_or_default();
                if workspace.is_empty() {
                    return Err(Status::invalid_argument(
                        "ChannelRegister must include workspace",
                    ));
                }
                let key = format!("{}-{}", reg.channel_type, reg.channel_name);
                (key, workspace)
            }
            _ => {
                return Err(Status::invalid_argument(
                    "first message must be ChannelRegister",
                ));
            }
        };

        let _ = state.get_or_create_workspace(&workspace).await;

        let (tx, rx) = mpsc::channel(16);
        let channel_key_clone = channel_key.clone();
        state.register_channel(channel_key.clone(), tx).await;

        tokio::spawn(async move {
            while let Ok(Some(inbound)) = stream.message().await {
                match inbound.event {
                    Some(channel_inbound::Event::UserMessage(msg)) => {
                        state
                            .notify_subscriber(
                                &workspace,
                                InboundMessage {
                                    content: msg.content,
                                    sender: msg.sender,
                                },
                            )
                            .await;
                    }
                    Some(channel_inbound::Event::Register(_)) => {}
                    None => {}
                }
            }
            state.unregister_channel(&channel_key_clone).await;
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
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let workspace = self.verify_workspace(&request).await?;

        let mut rx = self.state.subscribe_or_create(&workspace).await;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ConversationLog;
    use crate::state::ControllerState;
    use std::collections::HashMap;
    fn make_state() -> Arc<ControllerState> {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_dir = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let mut workspace_convs = HashMap::new();
        workspace_convs.insert(
            "default".to_string(),
            ConversationLog::new(&log_dir.join("default")),
        );
        Arc::new(ControllerState::new(
            workspace_convs,
            log_dir,
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
        ))
    }

    fn make_service() -> ControllerService {
        ControllerService::new(make_state(), None)
    }

    #[tokio::test]
    async fn turn_without_verifier_uses_default_workspace() {
        let state = make_state();
        let service = ControllerService::new(state.clone(), None);

        state
            .set_model_spec(
                "default".into(),
                crate::crd::TightbeamModelSpec {
                    format: "anthropic".into(),
                    model: "claude-sonnet-4-20250514".into(),
                    base_url: "https://api.anthropic.com/v1".into(),
                    thinking: None,
                    secret: None,
                },
            )
            .await;
        state.set_job_connected("default", true).await;

        let state_clone = state.clone();
        let consumer = tokio::spawn(async move {
            let pending = state_clone.wait_for_turn("default").await.unwrap();
            assert_eq!(pending.workspace, "default");
            pending
        });

        let request = Request::new(TurnRequest {
            system: Some("test".into()),
            tools: vec![],
            messages: vec![],
            agent: None,
            model: None,
        });

        let result = service.turn(request).await;
        assert!(result.is_ok());

        let pending = consumer.await.unwrap();
        assert_eq!(pending.workspace, "default");
        assert!(pending.reply_channel.is_none());
    }

    #[tokio::test]
    async fn list_models_returns_empty() {
        let service = make_service();
        let resp = service
            .list_models(Request::new(ListModelsRequest {}))
            .await
            .unwrap();
        assert!(resp.into_inner().models.is_empty());
    }
}
