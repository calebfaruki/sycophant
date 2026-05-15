use crate::state::{ControllerState, PendingTurn};
use futures::StreamExt;
use serde_json::Value;
use shared::auth::{extract_bearer_token, TokenVerifier};
use std::sync::Arc;

/// Seconds to keep the channel's outbound side open after the client
/// half-closes. Just under the 60s default gRPC client deadline.
const CHANNEL_DRAIN_SECS: u64 = 55;
use tightbeam_proto::convert::{
    chunk_to_turn_event, proto_message_to_provider, proto_tool_call_to_provider,
    provider_message_to_proto,
};
use tightbeam_proto::tightbeam_controller_server::TightbeamController;
use tightbeam_proto::{
    channel_inbound, channel_outbound, content_block, turn_result_chunk, ChannelInbound,
    ChannelOutbound, ChannelSend, EnrollRequest, EnrollResponse, GetTurnRequest,
    ListConversationsRequest, ListConversationsResponse, MintConversationRequest,
    MintConversationResponse,
    SubscribeRequest, TurnAck, TurnAssignment, TurnComplete, TurnEvent, TurnRequest,
    TurnResultChunk, TurnRole, UserMessage,
};
use tightbeam_providers::merge::merge_rfc7396;
use tightbeam_providers::types as provider;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

fn assistant_message_from_complete(complete: &TurnComplete) -> provider::Message {
    // Join every Text block in the response, in order.
    let collected_text: Vec<String> = complete
        .content
        .iter()
        .filter_map(|b| match &b.block {
            Some(content_block::Block::Text(t)) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    let text = if collected_text.is_empty() {
        None
    } else {
        Some(collected_text.join("\n"))
    };

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
    }
}

async fn build_params_json(
    state: &ControllerState,
    model: &str,
    frontmatter_params: Option<&serde_json::Map<String, Value>>,
) -> Option<String> {
    let model_spec = state.get_model_spec(model).await;
    let mut merged = model_spec.and_then(|s| s.params).unwrap_or_default();
    if let Some(fm_params) = frontmatter_params {
        merge_rfc7396(&mut merged, &Value::Object(fm_params.clone()));
    }
    if merged.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&merged)
                .expect("Map<String, Value> serializes deterministically"),
        )
    }
}

pub struct ControllerService {
    state: Arc<ControllerState>,
    verifier: Option<Arc<dyn TokenVerifier>>,
    signing_key: ed25519_dalek::SigningKey,
}

impl ControllerService {
    pub fn new(
        state: Arc<ControllerState>,
        verifier: Option<Arc<dyn TokenVerifier>>,
        signing_key: ed25519_dalek::SigningKey,
    ) -> Self {
        Self {
            state,
            verifier,
            signing_key,
        }
    }

    async fn verify_workspace<T>(&self, request: &Request<T>) -> Result<String, Status> {
        match &self.verifier {
            Some(v) => {
                let token = extract_bearer_token(request)?;
                v.verify_token(token).await
            }
            None => Err(Status::failed_precondition(
                "no token verifier configured: workspace identity cannot be established",
            )),
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
        if req.model_name.is_empty() {
            return Err(Status::invalid_argument(
                "GetTurnRequest.model_name must be set: the LLM Job must declare which model it serves",
            ));
        }
        let model = req.model_name;

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
                pending.conversation_id,
                pending.reply_channel,
                pending.role,
                pending.correlation_id,
                pending.system_prompt,
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
        let mut warnings_collected: Vec<String> = Vec::new();

        while let Some(chunk) = stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("stream error: {e}")))?
        {
            match &chunk.chunk {
                Some(turn_result_chunk::Chunk::Complete(_)) => {
                    complete_chunk = Some(chunk.clone());
                }
                Some(turn_result_chunk::Chunk::Warning(w)) => {
                    warnings_collected.push(w.field.clone());
                }
                _ => {}
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
            let tag =
                crate::conversation::derive_tag(active.role, active.correlation_id.as_deref());
            let attribution = crate::conversation::AssistantAttribution {
                model: Some(model.clone()),
                system_prompt_sha256: active
                    .system_prompt
                    .as_deref()
                    .map(crate::conversation::sha256_hex),
                warnings: warnings_collected.clone(),
            };
            let ws = self.state.get_or_create_workspace(&active.workspace).await;
            let conv_arc = match ws
                .get_or_create_conversation(&active.conversation_id)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("failed to load conversation: {e}");
                    return Ok(Response::new(TurnAck {}));
                }
            };
            let mut conv = conv_arc.write().await;
            let _ = conv.append_assistant_tagged(assistant_msg, tag, attribution).await;

            if complete.stop_reason == 1 && !matches!(active.role, Some(TurnRole::Delegate)) {
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

        if params.conversation_id.is_empty() {
            return Err(Status::invalid_argument(
                "TurnRequest.conversation_id must be set — call MintConversation to obtain one",
            ));
        }
        let conversation_id = params.conversation_id.clone();

        // Per-turn system prompt: each TurnRequest carries the system prompt the
        // dispatching call was running under. We do NOT retain it on the
        // workspace because orchestrator and delegate turns interleave under
        // different prompts; sharing one slot would cross-contaminate.
        //
        // The pre-strip value (`system`) is what gets hashed onto the audit log
        // entry — auditors hash canonical persona files directly with
        // `sha256sum` and the values match. The post-strip value
        // (`dispatch_system`) is what the LLM Job actually receives; the
        // frontmatter is metadata, not prompt content.
        let system = params.system.clone();
        let (dispatch_system, fm) = match system.as_deref() {
            Some(s) => {
                let (body, fm) = crate::conversation::strip_frontmatter(s);
                (Some(body), fm)
            }
            None => (None, crate::conversation::Frontmatter::default()),
        };

        let role = params.role.and_then(|r| TurnRole::try_from(r).ok());
        let scope = match (role, params.correlation_id.as_deref()) {
            (Some(TurnRole::Delegate), Some(id)) => crate::conversation::HistoryScope::Delegate(id),
            _ => crate::conversation::HistoryScope::Orchestrator,
        };

        let ws = self.state.get_or_create_workspace(&workspace).await;

        // Model resolution order:
        //   1. Frontmatter `model: inherit` → most recent assistant model in
        //      the current scope; falls through to (4) if no prior turn.
        //   2. Frontmatter `model: <name>` (any other value) → that name.
        //   3. `params.model` on the inbound TurnRequest (if non-empty).
        //   4. Reserved name `default` if registered, else alphabetic-first.
        let model = match fm.model.as_deref() {
            Some("inherit") => {
                let conv_arc = ws
                    .get_or_create_conversation(&conversation_id)
                    .await
                    .map_err(|e| {
                        Status::internal(format!("failed to load conversation: {e}"))
                    })?;
                let conv = conv_arc.read().await;
                let inherited = conv.last_assistant_model(scope);
                drop(conv);
                match inherited {
                    Some(m) => m,
                    None => self
                        .state
                        .default_or_alphabetic_first()
                        .await
                        .ok_or_else(|| {
                            Status::failed_precondition(
                                "model: inherit had no prior turn and no fallback model is registered",
                            )
                        })?,
                }
            }
            Some(other) => other.to_string(),
            None => match params.model.as_deref().filter(|m| !m.is_empty()) {
                Some(m) => m.to_string(),
                None => self
                    .state
                    .default_or_alphabetic_first()
                    .await
                    .ok_or_else(|| {
                        Status::failed_precondition(
                            "no model specified and no models registered: pass `model:` in frontmatter, set `model` on TurnRequest, or register at least one Model",
                        )
                    })?,
            },
        };

        tracing::info!(model = %model, workspace = %workspace, "turn: acquiring conversation write lock");
        let conv_arc = ws
            .get_or_create_conversation(&conversation_id)
            .await
            .map_err(|e| Status::internal(format!("failed to load conversation: {e}")))?;
        let mut conv = conv_arc.write().await;
        tracing::info!("turn: lock acquired");

        let job_action = self.state.check_job_needed(&model).await;
        if matches!(job_action, crate::state::JobAction::NoModelSpec) {
            return Err(Status::failed_precondition(format!(
                "no Model configured for '{model}'"
            )));
        }
        if let crate::state::JobAction::NoProviderSpec(ref provider_name) = job_action {
            return Err(Status::failed_precondition(format!(
                "Model '{model}' references missing provider '{provider_name}'"
            )));
        }

        match &job_action {
            crate::state::JobAction::AlreadyConnected => {
                tracing::debug!(model = %model, "reusing existing LLM job");
            }
            crate::state::JobAction::NoKubeClient => {
                tracing::error!(model = %model, "BUG: no kube client at request time");
            }
            crate::state::JobAction::Create(_)
            | crate::state::JobAction::NoModelSpec
            | crate::state::JobAction::NoProviderSpec(_) => {}
        }

        let incoming: Vec<provider::Message> = params
            .messages
            .iter()
            .map(proto_message_to_provider)
            .collect();

        let rollback_len = conv.len();

        let incoming_tag = crate::conversation::derive_tag(role, params.correlation_id.as_deref());

        conv.append_many_tagged(incoming, incoming_tag)
            .await
            .map_err(|e| Status::internal(format!("conversation append: {e}")))?;

        let history = conv.history_for_provider(scope);

        if let crate::state::JobAction::Create(create_spec) = job_action {
            let client = self.state.kube_client().unwrap();
            let addr = self.state.controller_addr().to_owned();
            let ns = self.state.namespace().to_owned();
            let image = self.state.llm_job_image().to_owned();

            tracing::info!(model = %model, "turn: no LLM Job connected, creating one");
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                crate::job::create_llm_job(
                    client,
                    &model,
                    &create_spec.model,
                    &create_spec.provider,
                    &image,
                    &addr,
                    &ns,
                    &workspace,
                    self.state.scheduling(),
                ),
            )
            .await
            {
                Ok(Ok(name)) => {
                    tracing::info!(job = %name, "turn: LLM Job created");
                }
                Ok(Err(e)) => {
                    tracing::error!("turn: k8s API rejected Job creation: {e}");
                    conv.truncate(rollback_len).await;
                    return Err(Status::internal(format!("failed to create LLM Job: {e}")));
                }
                Err(_) => {
                    tracing::error!("turn: k8s API timed out creating Job (10s)");
                    conv.truncate(rollback_len).await;
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
                conv.truncate(rollback_len).await;
                return Err(Status::deadline_exceeded(
                    "LLM Job did not connect within 30s",
                ));
            }
        }

        drop(conv);
        tracing::info!("turn: conversation lock released");

        tracing::info!("turn: building assignment");

        let proto_messages: Vec<_> = history.iter().map(provider_message_to_proto).collect();

        let params_json = build_params_json(&self.state, &model, fm.params.as_ref()).await;

        let assignment = TurnAssignment {
            // dispatch_system is the post-frontmatter-strip body; the LLM Job
            // sees this. Frontmatter is metadata (e.g., model selection), not
            // prompt content.
            system: dispatch_system,
            tools: params.tools,
            messages: proto_messages,
            params_json,
        };

        let (result_tx, result_rx) = mpsc::channel(64);
        let pending = PendingTurn {
            assignment,
            result_tx,
            workspace,
            conversation_id: conversation_id.clone(),
            reply_channel: params.reply_channel,
            role,
            correlation_id: params.correlation_id,
            // system is the pre-strip value; the audit hash on the assistant
            // log entry is computed from this so external auditors can match
            // log entries to canonical persona files via `sha256sum`.
            system_prompt: system,
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

    async fn mint_conversation(
        &self,
        request: Request<MintConversationRequest>,
    ) -> Result<Response<MintConversationResponse>, Status> {
        let _workspace = self.verify_workspace(&request).await?;
        let conversation_id = uuid::Uuid::new_v4().to_string();
        Ok(Response::new(MintConversationResponse { conversation_id }))
    }

    async fn list_conversations(
        &self,
        request: Request<ListConversationsRequest>,
    ) -> Result<Response<ListConversationsResponse>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        // The verifier-confirmed workspace claim wins over the request body;
        // the body's `workspace` is informational only and must agree.
        if !req.workspace.is_empty() && req.workspace != workspace {
            return Err(Status::permission_denied(
                "workspace claim does not match request body",
            ));
        }
        let ws = self.state.get_or_create_workspace(&workspace).await;
        let conversation_ids = ws.list_conversation_ids().await;
        Ok(Response::new(ListConversationsResponse { conversation_ids }))
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
                                UserMessage {
                                    content: msg.content,
                                    sender: msg.sender,
                                    reply_channel: Some(channel_key.clone()),
                                },
                            )
                            .await;
                    }
                    Some(channel_inbound::Event::Register(_)) => {}
                    None => {}
                }
            }
            // Keep the outbound channel alive for multi-turn responses.
            // CLI clients half-close immediately; the LLM response may
            // require tool_use → tool_result → end_turn (10-30s).
            tokio::time::sleep(std::time::Duration::from_secs(CHANNEL_DRAIN_SECS)).await;
            state.unregister_channel(&channel_key_clone).await;
        });

        #[allow(clippy::result_large_err)]
        let outbound_stream =
            ReceiverStream::new(rx).map(|msg| -> Result<ChannelOutbound, Status> { Ok(msg) });

        Ok(Response::new(Box::pin(outbound_stream)))
    }

    type SubscribeStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<UserMessage, Status>> + Send>>;

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

    async fn enroll_device(
        &self,
        request: Request<EnrollRequest>,
    ) -> Result<Response<EnrollResponse>, Status> {
        // EnrollDevice is unauthenticated by design — the enrollment code
        // itself is the authentication artifact. The signing key validates
        // the code's signature/expiry.
        let req = request.into_inner();
        let claims = shared::auth::verify_enrollment_code(
            &self.signing_key.verifying_key(),
            &req.enrollment_code,
        )?;

        let device_id = uuid::Uuid::new_v4().to_string();
        // 90-day device JWT lifetime per Phase 2 plan. No refresh; client
        // re-enrolls when expired.
        const DEVICE_JWT_TTL_SECS: i64 = 90 * 86_400;
        let (jwt, exp) = shared::auth::sign_device_jwt(
            &self.signing_key,
            &claims.workspace,
            &device_id,
            DEVICE_JWT_TTL_SECS,
        );

        tracing::info!(
            workspace = %claims.workspace,
            device_name = %claims.device_name,
            device_id = %device_id,
            "device enrolled"
        );

        Ok(Response::new(EnrollResponse {
            jwt,
            device_id,
            expires_at: exp,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ControllerState;
    use shared::auth::TokenVerifier;

    /// Test verifier that ignores the token and returns a fixed workspace
    /// name. Mirrors the integration test helper.
    struct FixedWorkspaceVerifier(String);

    #[tonic::async_trait]
    impl TokenVerifier for FixedWorkspaceVerifier {
        async fn verify_token(&self, _token: &str) -> Result<String, Status> {
            Ok(self.0.clone())
        }
    }

    fn fixed_verifier(name: &str) -> Arc<dyn TokenVerifier> {
        Arc::new(FixedWorkspaceVerifier(name.to_string()))
    }

    fn authed<T>(inner: T) -> Request<T> {
        let mut req = Request::new(inner);
        req.metadata_mut()
            .insert("authorization", "Bearer test".parse().unwrap());
        req
    }

    fn make_state() -> Arc<ControllerState> {
        use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
        // `into_path()` releases the TempDir's drop-time cleanup so the
        // directory survives this function's return. Intentional test-scoped
        // leak; process exit handles eventual cleanup.
        let log_dir = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> =
            Arc::new(LocalFsFactory::new(log_dir));
        Arc::new(ControllerState::new(
            factory,
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/llm-job:latest".into(),
            shared::scheduling::SchedulingConfig::default(),
        ))
    }

    /// Deterministic Ed25519 key for tests. Real callers load from the
    /// chart's mounted Secret; tests don't care about randomness.
    fn fixture_signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn assistant_message_concatenates_multiple_text_blocks() {
        use tightbeam_proto::{content_block, ContentBlock, StopReason, TextBlock, TurnComplete};
        let complete = TurnComplete {
            stop_reason: StopReason::EndTurn as i32,
            content: vec![
                ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock {
                        text: "first part".into(),
                    })),
                },
                ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock {
                        text: "second part".into(),
                    })),
                },
            ],
            tool_calls: vec![],
        };
        let msg = assistant_message_from_complete(&complete);
        let content = msg
            .content
            .expect("message must carry content");
        assert_eq!(content.len(), 1, "expected one merged text block");
        let provider::ContentBlock::Text { text } = &content[0] else {
            panic!("expected Text variant, got {:?}", content[0]);
        };
        // Pins the multi-block accumulation contract: both texts present,
        // joined by newline. Defends against regression to first-block-only
        // semantics.
        assert_eq!(text, "first part\nsecond part");
    }

    #[tokio::test]
    async fn turn_errors_when_no_verifier_configured() {
        // Replaces the old `turn_without_verifier_uses_default_workspace`
        // test, whose premise (silent fallback to workspace="default") was
        // the reserved-name anti-pattern this change deletes.
        let state = make_state();
        let service = ControllerService::new(state.clone(), None, fixture_signing_key());

        let result = service
            .turn(authed(TurnRequest {
                system: Some("test".into()),
                tools: vec![],
                messages: vec![],
                model: None,
                reply_channel: None,
                role: None,
                correlation_id: None,
                conversation_id: "test-conv".into(),
            }))
            .await;

        let status = match result {
            Ok(_) => panic!("turn must fail when no verifier configured"),
            Err(s) => s,
        };
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert!(
            status.message().contains("no token verifier configured"),
            "got: {:?}",
            status.message()
        );
    }

    #[tokio::test]
    async fn turn_with_reply_channel_propagates_to_pending() {
        let state = make_state();
        let service = ControllerService::new(
            state.clone(),
            Some(fixed_verifier("default")),
            fixture_signing_key(),
        );

        state
            .set_model_spec(
                "default".into(),
                crate::crd::ModelSpec {
                    provider_ref: crate::crd::ProviderRef {
                        name: "anthropic".into(),
                    },
                    model: "claude-sonnet-4-20250514".into(),
                    params: None,
                },
            )
            .await;
        state
            .set_provider_spec(
                "anthropic".into(),
                crate::crd::ProviderSpec {
                    format: "anthropic".into(),
                    base_url: Some("https://api.anthropic.com/v1".into()),
                    secret: crate::crd::ProviderSecret {
                        name: "anthropic-key".into(),
                        key: None,
                    },
                },
            )
            .await;
        state.set_job_connected("default", true).await;

        let state_clone = state.clone();
        let consumer =
            tokio::spawn(async move { state_clone.wait_for_turn("default").await.unwrap() });

        let request = authed(TurnRequest {
            system: Some("test".into()),
            tools: vec![],
            messages: vec![],
            model: None,
            reply_channel: Some("test-channel".into()),
            role: None,
            correlation_id: None,
            conversation_id: "test-conv".into(),
        });

        let result = service.turn(request).await;
        assert!(result.is_ok());

        let pending = consumer.await.unwrap();
        assert_eq!(
            pending.reply_channel.as_deref(),
            Some("test-channel"),
            "reply_channel must propagate from TurnRequest to PendingTurn"
        );
    }

    #[tokio::test]
    async fn params_json_none_when_neither_set() {
        let state = make_state();
        state
            .set_model_spec(
                "m".into(),
                crate::crd::ModelSpec {
                    provider_ref: crate::crd::ProviderRef {
                        name: "anthropic".into(),
                    },
                    model: "claude".into(),
                    params: None,
                },
            )
            .await;
        let result = build_params_json(&state, "m", None).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn params_json_carries_model_params_when_only_model_set() {
        let state = make_state();
        let mut params = serde_json::Map::new();
        params.insert("temperature".into(), serde_json::json!(0.7));
        state
            .set_model_spec(
                "m".into(),
                crate::crd::ModelSpec {
                    provider_ref: crate::crd::ProviderRef {
                        name: "anthropic".into(),
                    },
                    model: "claude".into(),
                    params: Some(params),
                },
            )
            .await;
        let result = build_params_json(&state, "m", None).await.expect("Some");
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed.get("temperature"), Some(&serde_json::json!(0.7)));
    }

    #[tokio::test]
    async fn params_json_merges_frontmatter_over_model_via_rfc7396() {
        let state = make_state();
        let mut model_params = serde_json::Map::new();
        model_params.insert("output_config".into(), serde_json::json!({"effort": "low"}));
        state
            .set_model_spec(
                "m".into(),
                crate::crd::ModelSpec {
                    provider_ref: crate::crd::ProviderRef {
                        name: "anthropic".into(),
                    },
                    model: "claude".into(),
                    params: Some(model_params),
                },
            )
            .await;

        let mut fm_params = serde_json::Map::new();
        fm_params.insert("output_config".into(), serde_json::json!({"effort": "max"}));

        let result = build_params_json(&state, "m", Some(&fm_params))
            .await
            .expect("Some");
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        // RFC 7396 recursive merge: frontmatter wins for `effort`.
        assert_eq!(
            parsed.get("output_config").and_then(|v| v.get("effort")),
            Some(&serde_json::json!("max"))
        );
    }
}
