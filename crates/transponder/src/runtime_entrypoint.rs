//! Per-user-message runtime loop.
//!
//! Each iteration:
//! 1. Read the primary persona (`AGENTS.md`) from a shared cache populated
//!    by `watch_mainframe_agent` (defined below). The cache decouples the
//!    hot path from transient mainframe RPC failures — a single get_agent
//!    blip no longer drops the inbound user message.
//! 2. Build a `TurnRequest` with `system = persona`, the new user message,
//!    and the full tool set. Anthropic treats each request as stateless,
//!    so tools must be sent on every turn — `agent::llm_loop` propagates
//!    them through tool-result continuations and nudges.
//! 3. Hand to `agent::llm_loop`. Terminal-state policy (warn-and-continue
//!    on `Halt::*`, propagate on infra failures) is decided by
//!    `handle_llm_loop_result` at this layer.

use std::sync::Arc;

use tightbeam_proto::{Message, ToolDefinition, TurnRequest};
use tokio::sync::Mutex;

use crate::agent::{self, LoopError, LoopHalt, LoopMode};
use crate::clients::{MainframeClient, TightbeamClient};
use crate::message_source::MessageSource;
use crate::tool_router::ToolRouter;

/// Refresh the primary `AGENTS.md` from mainframe-ctrl into a shared cache,
/// independent of the per-turn message loop. Survives transient RPC
/// failures: on success refreshes every 30s; on failure backs off 2s and
/// retries. The cache stays populated with the most recent good value, so
/// the orchestrator can keep processing messages through a mainframe blip.
pub(crate) async fn watch_mainframe_agent(
    mut client: MainframeClient,
    cache: Arc<Mutex<String>>,
    initial_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    let mut initial_tx = initial_tx;
    loop {
        match client.get_agent("").await {
            Ok(persona) => {
                {
                    let mut guard = cache.lock().await;
                    *guard = persona;
                }
                if let Some(tx) = initial_tx.take() {
                    let _ = tx.send(());
                }
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "mainframe get_agent failed, retrying");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

pub(crate) async fn message_loop(
    max_iterations: u32,
    tightbeam: &mut TightbeamClient,
    agent_cache: Arc<Mutex<String>>,
    tool_router: Arc<ToolRouter>,
    message_source: &mut dyn MessageSource,
) -> Result<(), String> {
    // Conversation lifecycle now lives on the client. Each inbound
    // user message carries the conversation_id tightbeam stamped at
    // ingest time; pod restart no longer loses thread context.
    loop {
        let inbound = message_source.next_message().await?;
        let tool_defs = tool_router.tool_definitions();

        // Read the primary persona from the cache. The agent watcher
        // populates this on startup (via initial_tx) and refreshes it
        // every 30s; a transient mainframe RPC failure between refreshes
        // leaves the previous good value in place.
        let persona = agent_cache.lock().await.clone();
        tracing::info!(bytes = persona.len(), "fetched primary persona");

        let reply_channel = inbound.reply_channel.clone();
        let request = build_turn_request(
            &persona,
            inbound.content,
            &tool_defs,
            inbound.reply_channel,
            inbound.conversation_id,
        );
        let result = agent::llm_loop(
            max_iterations,
            tightbeam,
            &*tool_router,
            request,
            LoopMode { reply_channel },
        )
        .await;
        handle_llm_loop_result(result)?;
    }
}

/// Map an `llm_loop` result to a decision the message loop can act on. `Ok`
/// means "continue waiting for the next user message" (whether the loop
/// completed naturally or hit a `Halt::*` we choose to swallow);
/// `Err(String)` means "infrastructure failure — propagate to `main.rs`
/// so the pod restarts."
fn handle_llm_loop_result(result: Result<String, LoopError>) -> Result<(), String> {
    match result {
        Ok(_) => Ok(()),
        Err(LoopError::Halt(LoopHalt::IterationLimit { limit })) => {
            tracing::warn!(limit, "iteration limit reached, awaiting next user message");
            Ok(())
        }
        Err(LoopError::Halt(LoopHalt::MaxTokens(partial))) => {
            tracing::warn!(
                partial_bytes = partial.len(),
                "max_tokens reached, awaiting next user message"
            );
            Ok(())
        }
        Err(LoopError::Halt(LoopHalt::UnknownStop(reason))) => {
            tracing::warn!(
                ?reason,
                "unexpected stop reason, awaiting next user message"
            );
            Ok(())
        }
        Err(LoopError::TightbeamRpc(e))
        | Err(LoopError::ToolDispatch(e))
        | Err(LoopError::StreamEnded(e)) => Err(e),
    }
}

fn build_turn_request(
    persona: &str,
    user_content: Vec<tightbeam_proto::ContentBlock>,
    tool_defs: &[ToolDefinition],
    reply_channel: Option<String>,
    conversation_id: String,
) -> TurnRequest {
    TurnRequest {
        system: Some(persona.to_string()),
        tools: tool_defs.to_vec(),
        messages: vec![Message {
            role: "user".into(),
            content: user_content,
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        }],
        model: None,
        reply_channel,
        role: None,
        correlation_id: None,
        conversation_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tightbeam_proto::{content_block, ContentBlock, TextBlock, ToolDefinition};

    fn user_text(s: &str) -> Vec<ContentBlock> {
        vec![ContentBlock {
            block: Some(content_block::Block::Text(TextBlock { text: s.into() })),
        }]
    }

    #[test]
    fn handle_result_swallows_iteration_limit() {
        let res =
            handle_llm_loop_result(Err(LoopError::Halt(LoopHalt::IterationLimit { limit: 7 })));
        assert!(res.is_ok());
    }

    #[test]
    fn handle_result_swallows_max_tokens_with_partial_text() {
        let res =
            handle_llm_loop_result(Err(LoopError::Halt(LoopHalt::MaxTokens("partial".into()))));
        assert!(res.is_ok());
    }

    #[test]
    fn handle_result_swallows_unknown_stop() {
        let res = handle_llm_loop_result(Err(LoopError::Halt(LoopHalt::UnknownStop(
            tightbeam_proto::StopReason::Unspecified,
        ))));
        assert!(res.is_ok());
    }

    #[test]
    fn handle_result_propagates_tightbeam_rpc() {
        let res = handle_llm_loop_result(Err(LoopError::TightbeamRpc("boom".into())));
        assert_eq!(res.unwrap_err(), "boom");
    }

    #[test]
    fn handle_result_propagates_stream_ended() {
        let res = handle_llm_loop_result(Err(LoopError::StreamEnded("eof".into())));
        assert_eq!(res.unwrap_err(), "eof");
    }

    #[test]
    fn handle_result_passes_through_ok() {
        let res = handle_llm_loop_result(Ok("final text".into()));
        assert!(res.is_ok());
    }

    #[test]
    fn build_turn_request_carries_full_tool_set() {
        let tool_defs = vec![ToolDefinition {
            name: "Bash".into(),
            description: "run shell".into(),
            parameters_json: "{}".into(),
        }];
        let req = build_turn_request(
            "PERSONA",
            user_text("hello"),
            &tool_defs,
            Some("test-channel".into()),
            "test-conv".into(),
        );

        assert_eq!(req.system.as_deref(), Some("PERSONA"));
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "Bash");
        assert_eq!(req.role, None);
        assert_eq!(req.reply_channel.as_deref(), Some("test-channel"));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
    }

    #[test]
    fn persona_passed_in_lands_on_request_system() {
        let req = build_turn_request(
            "the persona text",
            user_text("hi"),
            &[],
            None,
            "conv".into(),
        );
        assert_eq!(req.system.as_deref(), Some("the persona text"));
    }
}
