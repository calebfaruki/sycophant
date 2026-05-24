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
    channel_inbound, channel_outbound, content_block, turn_result_chunk, ChannelAck,
    ChannelInbound, ChannelIngestAck, ChannelIngestRequest, ChannelOutbound, ChannelReceiveRequest,
    ChannelSend, GetConversationHistoryRequest, GetConversationHistoryResponse, GetTurnRequest,
    HistoryEntry, ListConversationsRequest, ListConversationsResponse, MintConversationRequest,
    MintConversationResponse, RedeemEnrollmentRequest, RedeemEnrollmentResponse, SubscribeRequest,
    TurnAck, TurnAssignment, TurnComplete, TurnEvent, TurnRequest, TurnResultChunk, TurnRole,
    UserMessage,
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

/// Per-listener strategy for resolving the caller's workspace.
/// Constructed by the listener wiring in `main.rs`; the handler is
/// listener-agnostic.
pub enum VerificationStrategy {
    /// Internal listener (port 9090): K8s SA token in `authorization`
    /// metadata, verified via TokenReview.
    BearerToken(Arc<dyn TokenVerifier>),
    /// External listener (port 9091): the `signature_layer` tower
    /// middleware has already verified the signed-request envelope
    /// and stamped the workspace on the request extensions. The
    /// handler trusts the extension.
    TrustExtensionsSetByMiddleware,
    /// Misconfigured — no verifier wired. Every authenticated RPC
    /// fails with FailedPrecondition.
    None,
}

pub struct ControllerService {
    state: Arc<ControllerState>,
    strategy: VerificationStrategy,
    signing_key: ed25519_dalek::SigningKey,
}

impl ControllerService {
    /// Construct a controller service for the internal listener.
    /// `verifier` is the K8s TokenReview verifier (None when no kube
    /// client is available — controller will reject all authed RPCs).
    pub fn internal(
        state: Arc<ControllerState>,
        verifier: Option<Arc<dyn TokenVerifier>>,
        signing_key: ed25519_dalek::SigningKey,
    ) -> Self {
        let strategy = match verifier {
            Some(v) => VerificationStrategy::BearerToken(v),
            None => VerificationStrategy::None,
        };
        Self {
            state,
            strategy,
            signing_key,
        }
    }

    /// Construct a controller service for the external listener. The
    /// signature-verifying middleware in `signature_layer` is
    /// responsible for proving the caller's identity; the handler
    /// reads the verified workspace from request extensions.
    pub fn external(state: Arc<ControllerState>, signing_key: ed25519_dalek::SigningKey) -> Self {
        Self {
            state,
            strategy: VerificationStrategy::TrustExtensionsSetByMiddleware,
            signing_key,
        }
    }

    async fn verify_workspace<T>(&self, request: &Request<T>) -> Result<String, Status> {
        match &self.strategy {
            VerificationStrategy::BearerToken(v) => {
                let token = extract_bearer_token(request)?;
                v.verify_token(token).await
            }
            VerificationStrategy::TrustExtensionsSetByMiddleware => request
                .extensions()
                .get::<crate::signature_layer::VerifiedWorkspace>()
                .map(|w| w.0.clone())
                .ok_or_else(|| {
                    Status::permission_denied(
                        "missing verified workspace extension; middleware must populate it",
                    )
                }),
            VerificationStrategy::None => Err(Status::failed_precondition(
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
        // Caller MUST authenticate via SA token. The LLM Job pod runs
        // with sa-<workspace> so its identity binds to a specific
        // workspace; we verify the dequeued PendingTurn's workspace
        // matches before handing over the assignment (which carries
        // system prompt + conversation history).
        let caller_workspace = self.verify_workspace(&request).await?;
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

        // ModelSlot is keyed globally by model name (a model spec may be
        // referenced by multiple workspaces). The workspace-binding check
        // is on the dequeued PendingTurn, not the request — the caller's
        // SA-derived workspace must own the turn it's about to receive.
        if pending.workspace != caller_workspace {
            tracing::warn!(
                model = %model,
                caller = %caller_workspace,
                pending_owner = %pending.workspace,
                "get_turn: caller workspace mismatch — refusing assignment"
            );
            return Err(Status::permission_denied(
                "LLM Job SA workspace does not match the workspace that enqueued this turn",
            ));
        }

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
            let conv_arc = match ws.get_or_create_conversation(&active.conversation_id).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("failed to load conversation: {e}");
                    return Ok(Response::new(TurnAck {}));
                }
            };
            let mut conv = conv_arc.write().await;
            let _ = conv
                .append_assistant_tagged(assistant_msg, tag, attribution)
                .await;

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
        if !params.conversation_id.starts_with(&format!("{workspace}.")) {
            return Err(Status::permission_denied(
                "TurnRequest.conversation_id workspace prefix does not match caller",
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
        let workspace = self.verify_workspace(&request).await?;
        let conversation_id = format!("{workspace}.{}", uuid::Uuid::new_v4());
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
        Ok(Response::new(ListConversationsResponse {
            conversation_ids,
        }))
    }

    type ChannelStreamStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChannelOutbound, Status>> + Send>>;

    async fn channel_stream(
        &self,
        request: Request<Streaming<ChannelInbound>>,
    ) -> Result<Response<Self::ChannelStreamStream>, Status> {
        // ChannelStream is internal-listener-only (rejected by the
        // signature middleware on the external listener — streaming
        // requests can't pass the body-collect verify). Auth here is
        // K8s SA token. The verified workspace MUST match the workspace
        // the client claims in ChannelRegister; mismatch means an
        // in-cluster pod is trying to inject into a different
        // workspace's stream.
        //
        // Extract the bearer token BEFORE consuming the request; the
        // Streaming<ChannelInbound> inner type isn't Sync, so we can't
        // borrow the Request across `await`. We re-implement the
        // BearerToken-strategy verify inline.
        let caller_workspace = match &self.strategy {
            VerificationStrategy::BearerToken(v) => {
                let token = extract_bearer_token(&request)?.to_string();
                v.verify_token(&token).await?
            }
            VerificationStrategy::TrustExtensionsSetByMiddleware => request
                .extensions()
                .get::<crate::signature_layer::VerifiedWorkspace>()
                .map(|w| w.0.clone())
                .ok_or_else(|| {
                    Status::permission_denied(
                        "missing verified workspace extension; middleware must populate it",
                    )
                })?,
            VerificationStrategy::None => {
                return Err(Status::failed_precondition(
                    "no token verifier configured: workspace identity cannot be established",
                ));
            }
        };
        let mut stream = request.into_inner();
        let state = self.state.clone();

        let first = stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("stream error: {e}")))?
            .ok_or_else(|| Status::invalid_argument("empty stream"))?;

        let (adapter_hint, workspace) = match first.event {
            Some(channel_inbound::Event::Register(reg)) => {
                let claimed = reg.workspace.unwrap_or_default();
                if claimed.is_empty() {
                    return Err(Status::invalid_argument(
                        "ChannelRegister must include workspace",
                    ));
                }
                if claimed != caller_workspace {
                    return Err(Status::permission_denied(
                        "ChannelRegister.workspace does not match caller's authenticated workspace",
                    ));
                }
                (reg.adapter_hint, claimed)
            }
            _ => {
                return Err(Status::invalid_argument(
                    "first message must be ChannelRegister",
                ));
            }
        };

        let _ = state.get_or_create_workspace(&workspace).await;

        let (tx, rx) = mpsc::channel(16);
        // Mint the server-side channel_id. Send it back as the first
        // ChannelOutbound frame (ChannelAck) so the adapter knows what
        // to echo on subsequent UserMessage frames as well as on any
        // out-of-band ChannelIngest from external clients sharing the
        // same workspace (in-cluster adapters typically don't use that
        // path, but the contract is uniform).
        let channel_id = state
            .mint_channel(workspace.clone(), adapter_hint, tx.clone())
            .await;
        let channel_id_for_drop = channel_id.clone();
        let channel_id_for_loop = channel_id.clone();

        if let Err(e) = tx
            .send(ChannelOutbound {
                command: Some(channel_outbound::Command::Ack(ChannelAck {
                    channel_id: channel_id.clone(),
                })),
            })
            .await
        {
            tracing::warn!(?e, "channel_stream: failed to send initial ChannelAck");
        }

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
                                    reply_channel: Some(channel_id_for_loop.clone()),
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
            state.unregister_channel(&channel_id_for_drop).await;
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

    async fn redeem_enrollment(
        &self,
        request: Request<RedeemEnrollmentRequest>,
    ) -> Result<Response<RedeemEnrollmentResponse>, Status> {
        // Unauthenticated by design — the signed enrollment code IS the
        // authentication artifact. Business logic + single-use guard live
        // in `client_store::redeem_for_client` so the security-critical
        // branches are unit-tested behind the `ClientStore` interface.
        let req = request.into_inner();
        let claims = shared::auth::verify_enrollment_code(
            &self.signing_key.verifying_key(),
            &req.enrollment_code,
        )?;

        let kube_client = self
            .state
            .kube_client()
            .ok_or_else(|| Status::failed_precondition("controller has no kube client"))?
            .clone();
        let store = crate::client_store::KubeClientStore::new(kube_client, self.state.namespace());
        let resp = crate::client_store::redeem_for_client(&store, &claims, &req.public_key).await?;

        tracing::info!(
            workspace = %claims.workspace,
            client = %resp.client_name,
            "client enrolled"
        );

        Ok(Response::new(resp))
    }

    async fn get_conversation_history(
        &self,
        request: Request<GetConversationHistoryRequest>,
    ) -> Result<Response<GetConversationHistoryResponse>, Status> {
        // Backs the transponder's `recent_turns` built-in tool. Workspace
        // is derived from the calling SA token via `verify_workspace`;
        // the conversation_id must belong to that workspace (current
        // store layout is per-workspace, so the path naturally scopes).
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        if req.conversation_id.is_empty() {
            return Err(Status::invalid_argument("conversation_id required"));
        }
        if !req.conversation_id.starts_with(&format!("{workspace}.")) {
            return Err(Status::permission_denied(
                "conversation_id workspace prefix does not match caller",
            ));
        }

        let limit = effective_history_limit(req.limit);
        let ws = self.state.get_or_create_workspace(&workspace).await;
        let conv = ws
            .get_or_create_conversation(&req.conversation_id)
            .await
            .map_err(|e| Status::internal(format!("load conversation: {e}")))?;
        let snap = conv.read().await.snapshot(limit);
        let truncated = (snap.entries.len() as u64) < snap.total_seq;
        let entries: Vec<HistoryEntry> = snap
            .entries
            .into_iter()
            .map(|e| HistoryEntry {
                seq: e.seq,
                ts: e.ts,
                message: Some(provider_message_to_proto(&e.message)),
                tag: e.tag,
            })
            .collect();

        Ok(Response::new(GetConversationHistoryResponse {
            entries,
            total_seq: snap.total_seq,
            truncated,
        }))
    }

    async fn channel_ingest(
        &self,
        request: Request<ChannelIngestRequest>,
    ) -> Result<Response<ChannelIngestAck>, Status> {
        // Workspace is derived from the caller's signature (external
        // listener) or SA token (internal listener) — NEVER from the
        // request body. The caller echoes the server-minted channel_id
        // received earlier from ChannelReceive's first ChannelAck frame;
        // we verify it belongs to the caller's verified workspace before
        // routing.
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        if req.channel_id.is_empty() {
            return Err(Status::invalid_argument(
                "ChannelIngestRequest.channel_id required",
            ));
        }
        match self.state.channel_workspace(&req.channel_id).await {
            Some(bound) if bound == workspace => {}
            Some(_) => {
                return Err(Status::permission_denied(
                    "ChannelIngestRequest.channel_id is bound to a different workspace",
                ));
            }
            None => {
                return Err(Status::not_found(
                    "ChannelIngestRequest.channel_id is not registered (call ChannelReceive first)",
                ));
            }
        }
        let user_message = req.user_message.ok_or_else(|| {
            Status::invalid_argument("ChannelIngestRequest.user_message required")
        })?;

        let _ = self.state.get_or_create_workspace(&workspace).await;
        // Stamp the reply-channel on the message so the transponder's
        // outbound reaches the matching ChannelReceive stream.
        self.state
            .notify_subscriber(
                &workspace,
                UserMessage {
                    content: user_message.content,
                    sender: user_message.sender,
                    reply_channel: Some(req.channel_id.clone()),
                },
            )
            .await;
        // conversation_id is reserved for future GetConversationHistory
        // replay; the transponder owns the live conversation_id and we
        // don't currently surface it back through here. Empty string is
        // the documented "unknown / not yet wired" value.
        Ok(Response::new(ChannelIngestAck {
            channel_id: req.channel_id,
            conversation_id: String::new(),
        }))
    }

    type ChannelReceiveStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChannelOutbound, Status>> + Send>>;

    async fn channel_receive(
        &self,
        request: Request<ChannelReceiveRequest>,
    ) -> Result<Response<Self::ChannelReceiveStream>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();
        let _ = self.state.get_or_create_workspace(&workspace).await;

        let (tx, rx) = mpsc::channel(16);
        // Mint the server-side channel_id, bind it to the verified
        // workspace, and stash the adapter_hint for log emission.
        let channel_id = self
            .state
            .mint_channel(workspace.clone(), req.adapter_hint.clone(), tx.clone())
            .await;
        tracing::info!(
            channel_id = %channel_id,
            workspace = %workspace,
            adapter_hint = ?req.adapter_hint,
            "channel_receive: minted channel_id"
        );

        // First frame of the outbound stream IS the ChannelAck — the
        // adapter MUST consume this before it can echo channel_id on
        // ChannelIngest.
        if let Err(e) = tx
            .send(ChannelOutbound {
                command: Some(channel_outbound::Command::Ack(ChannelAck {
                    channel_id: channel_id.clone(),
                })),
            })
            .await
        {
            tracing::warn!(?e, "channel_receive: failed to send initial ChannelAck");
        }

        // Unregister on stream drop. The drain delay matches the
        // channel_stream teardown so multi-frame outbound replies (10-30s)
        // can finish even if the client half-closes early.
        let state = self.state.clone();
        let channel_id_for_drop = channel_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(CHANNEL_DRAIN_SECS)).await;
            state.unregister_channel(&channel_id_for_drop).await;
        });

        #[allow(clippy::result_large_err)]
        let outbound_stream =
            ReceiverStream::new(rx).map(|msg| -> Result<ChannelOutbound, Status> { Ok(msg) });

        Ok(Response::new(Box::pin(outbound_stream)))
    }
}

/// Server-side clamp for `GetConversationHistoryRequest.limit`. `None`
/// or `Some(0)` → no limit (snapshot returns full log); positive
/// values are clamped to `MAX_HISTORY_LIMIT`. Extracted so the
/// clamping behavior is unit-testable.
const MAX_HISTORY_LIMIT: usize = 500;

fn effective_history_limit(requested: Option<u32>) -> Option<usize> {
    match requested {
        None | Some(0) => None,
        Some(n) => Some((n as usize).min(MAX_HISTORY_LIMIT)),
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
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(log_dir));
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
        let content = msg.content.expect("message must carry content");
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
        let service = ControllerService::internal(state.clone(), None, fixture_signing_key());

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
    async fn external_service_subscribe_reads_workspace_from_extension() {
        // External listener: signature_layer middleware has already
        // verified the signed-request envelope and stamped the
        // workspace on the request extensions. The handler trusts the
        // extension.
        use crate::signature_layer::VerifiedWorkspace;
        let state = make_state();
        let service = ControllerService::external(state.clone(), fixture_signing_key());

        let mut req = Request::new(SubscribeRequest {});
        req.extensions_mut()
            .insert(VerifiedWorkspace("ws-from-ext".to_string()));

        let response = service.subscribe(req).await;
        assert!(
            response.is_ok(),
            "external strategy must accept request with VerifiedWorkspace extension"
        );
    }

    #[tokio::test]
    async fn external_service_subscribe_rejects_missing_extension() {
        // No middleware → no extension. Handler must refuse rather
        // than silently default a workspace.
        let state = make_state();
        let service = ControllerService::external(state, fixture_signing_key());

        let req = Request::new(SubscribeRequest {});
        let err = match service.subscribe(req).await {
            Ok(_) => panic!("subscribe must reject missing extension"),
            Err(s) => s,
        };
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn external_service_ignores_bearer_token_metadata() {
        // External listener uses extensions only — a bearer-token
        // header must NOT be consulted (defends against a refactor
        // that accidentally folds in extract_bearer_token).
        let state = make_state();
        let service = ControllerService::external(state, fixture_signing_key());

        let req = authed(SubscribeRequest {}); // bearer token set, no extension
        let err = match service.subscribe(req).await {
            Ok(_) => panic!("bearer-token presence must not satisfy external strategy"),
            Err(s) => s,
        };
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn effective_history_limit_none_means_no_limit() {
        assert_eq!(effective_history_limit(None), None);
    }

    #[test]
    fn effective_history_limit_zero_means_no_limit() {
        // Proto convention: unset / 0 → no cap. Catches a regression
        // to treating `Some(0)` as "return zero entries."
        assert_eq!(effective_history_limit(Some(0)), None);
    }

    #[test]
    fn effective_history_limit_under_cap_passes_through() {
        assert_eq!(effective_history_limit(Some(42)), Some(42));
    }

    #[test]
    fn effective_history_limit_at_cap_passes_through() {
        assert_eq!(
            effective_history_limit(Some(MAX_HISTORY_LIMIT as u32)),
            Some(MAX_HISTORY_LIMIT),
        );
    }

    #[test]
    fn effective_history_limit_over_cap_clamps() {
        // Catches `<` vs `<=` and `min` vs `max` mutations on the clamp.
        assert_eq!(
            effective_history_limit(Some(MAX_HISTORY_LIMIT as u32 + 1)),
            Some(MAX_HISTORY_LIMIT),
        );
        assert_eq!(
            effective_history_limit(Some(u32::MAX)),
            Some(MAX_HISTORY_LIMIT),
        );
    }

    #[tokio::test]
    async fn get_conversation_history_rejects_empty_conversation_id() {
        let state = make_state();
        let service = ControllerService::internal(
            state,
            Some(fixed_verifier("default")),
            fixture_signing_key(),
        );
        let err = service
            .get_conversation_history(authed(GetConversationHistoryRequest {
                conversation_id: String::new(),
                limit: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn get_conversation_history_returns_empty_for_fresh_conversation() {
        // Fresh conversation auto-creates an empty log; snapshot returns
        // empty entries with total_seq=0 and truncated=false.
        let state = make_state();
        let service = ControllerService::internal(
            state,
            Some(fixed_verifier("default")),
            fixture_signing_key(),
        );
        let resp = service
            .get_conversation_history(authed(GetConversationHistoryRequest {
                conversation_id: "default.fresh-conv".into(),
                limit: None,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.entries.is_empty());
        assert_eq!(resp.total_seq, 0);
        assert!(!resp.truncated);
    }

    #[tokio::test]
    async fn get_conversation_history_without_verifier_returns_failed_precondition() {
        // Same auth gate every other authed RPC uses; pin it here so a
        // misrouted call to the unauthenticated-path test harness can't
        // sneak through.
        let state = make_state();
        let service = ControllerService::internal(state, None, fixture_signing_key());
        let err = service
            .get_conversation_history(authed(GetConversationHistoryRequest {
                conversation_id: "any".into(),
                limit: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn turn_with_reply_channel_propagates_to_pending() {
        let state = make_state();
        let service = ControllerService::internal(
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
            conversation_id: "default.test-conv".into(),
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
