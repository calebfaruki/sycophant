use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use pkm_proto::pkm_service_server::PkmService;
use pkm_proto::{
    pkm_event, transponder_event, PkmEvent, ResolveError, RunAgentTurn, RunSystemTurn,
    TransponderEvent,
};
use sycophant_auth::{extract_bearer_token, TokenVerifier};
use tightbeam_proto::Message;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::router_response;
use crate::state::PkmState;

const REPORT_TIMEOUT: Duration = Duration::from_secs(60);

pub struct PkmServiceImpl {
    state: Arc<PkmState>,
    verifier: Option<Arc<dyn TokenVerifier>>,
}

impl PkmServiceImpl {
    pub fn new(state: Arc<PkmState>, verifier: Option<Arc<dyn TokenVerifier>>) -> Self {
        Self { state, verifier }
    }

    /// Extract bearer token synchronously, then verify async without holding
    /// `&Request<T>` across await points (required because `Streaming<T>` is
    /// not `Sync`).
    async fn verify_workspace_owned(&self, token: Option<String>) -> Result<String, Status> {
        match (&self.verifier, token) {
            (Some(v), Some(t)) => v.verify_token(&t).await,
            (Some(_), None) => Err(Status::permission_denied("missing authorization metadata")),
            (None, _) => Ok("default".to_string()),
        }
    }
}

#[tonic::async_trait]
impl PkmService for PkmServiceImpl {
    type ResolveTurnStream = Pin<Box<dyn futures::Stream<Item = Result<PkmEvent, Status>> + Send>>;

    async fn resolve_turn(
        &self,
        request: Request<Streaming<TransponderEvent>>,
    ) -> Result<Response<Self::ResolveTurnStream>, Status> {
        // Extract token synchronously before consuming the request — Streaming<T>
        // is not Sync so we cannot hold &request across an await.
        let token = if self.verifier.is_some() {
            Some(extract_bearer_token(&request)?.to_string())
        } else {
            None
        };
        let mut stream = request.into_inner();
        let workspace_id = self.verify_workspace_owned(token).await?;

        // First inbound event must be UserMessage. Synchronous validation
        // returns a Status to the client before the response stream opens.
        let first = stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("stream error: {e}")))?
            .ok_or_else(|| Status::invalid_argument("empty stream"))?;

        let user_msg = match first.event {
            Some(transponder_event::Event::UserMessage(um)) => um,
            _ => {
                return Err(Status::invalid_argument(
                    "first message must be UserMessage",
                ));
            }
        };

        let state = self.state.clone();
        let fallback_agent = state.active_agent(&workspace_id).await;
        let router_prompt = state.router_prompt().to_string();
        let schema = state.select_agent_schema();

        let system_msg = Message {
            role: "user".into(),
            content: user_msg.content,
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
            agent: None,
        };

        // Buffer 1: strict request-response. Each tx.send().await must flush
        // before the next stream.message().await.
        let (tx, rx) = mpsc::channel::<Result<PkmEvent, Status>>(1);

        tokio::spawn(async move {
            // 1. Yield RunSystemTurn
            let run_system = PkmEvent {
                event: Some(pkm_event::Event::RunSystemTurn(RunSystemTurn {
                    system_prompt: router_prompt,
                    messages: vec![system_msg],
                    response_schema_json: Some(schema),
                })),
            };
            if tx.send(Ok(run_system)).await.is_err() {
                return;
            }

            // 2. Await ReportSystemTurn with timeout. tx.closed() defends
            // against tonic 0.13 stream-leak issue #2079.
            let next = tokio::select! {
                msg = stream.message() => msg,
                _ = tx.closed() => return,
                _ = tokio::time::sleep(REPORT_TIMEOUT) => {
                    let _ = tx.send(Ok(resolve_error(
                        1,
                        "timeout awaiting ReportSystemTurn",
                    ))).await;
                    return;
                }
            };

            let report = match next {
                Ok(Some(TransponderEvent {
                    event: Some(transponder_event::Event::ReportSystemTurn(r)),
                })) => r,
                Ok(Some(_)) => {
                    let _ = tx
                        .send(Ok(resolve_error(2, "expected ReportSystemTurn")))
                        .await;
                    return;
                }
                Ok(None) | Err(_) => return,
            };

            // 3. Pick agent: prefer structured_json (schema-validated) over
            // free-text fallback. The provider's strict-mode enforcement
            // should make structured_json's agent_name always valid, but we
            // still fall back on parse failure or unknown name.
            let picked = pick_agent_from_report(&report, state.prompts(), &fallback_agent);
            state.set_active_agent(&workspace_id, &picked).await;

            // 4. Yield terminal RunAgentTurn. Stream closes when this task ends.
            let agent_prompt = state
                .agent_prompt(&picked)
                .map(|s| s.to_string())
                .unwrap_or_default();
            let _ = tx
                .send(Ok(PkmEvent {
                    event: Some(pkm_event::Event::RunAgentTurn(RunAgentTurn {
                        agent_name: picked,
                        system_prompt: agent_prompt,
                        system_messages: vec![],
                    })),
                }))
                .await;
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

fn resolve_error(code: i32, message: &str) -> PkmEvent {
    PkmEvent {
        event: Some(pkm_event::Event::ResolveError(ResolveError {
            code,
            message: message.to_string(),
        })),
    }
}

fn pick_agent_from_report(
    report: &pkm_proto::ReportSystemTurn,
    prompts: &std::collections::HashMap<String, String>,
    fallback: &str,
) -> String {
    if let Some(structured) = report.structured_json.as_deref() {
        match serde_json::from_str::<serde_json::Value>(structured) {
            Ok(v) => {
                if let Some(name) = v.get("agent_name").and_then(|n| n.as_str()) {
                    if prompts.contains_key(name) {
                        tracing::info!(agent = %name, "picked agent via structured_json");
                        return name.to_string();
                    }
                    tracing::warn!(
                        chosen = %name,
                        fallback = %fallback,
                        "structured_json agent_name not in prompts; falling back"
                    );
                } else {
                    tracing::warn!(
                        structured = %structured,
                        "structured_json missing agent_name; falling back to free-text parse"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    structured = %structured,
                    error = %e,
                    "structured_json failed to parse; falling back to free-text parse"
                );
            }
        }
    }
    router_response::parse(&report.response_json, prompts, fallback)
}
