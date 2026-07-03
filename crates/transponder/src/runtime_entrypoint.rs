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

use hangar_proto::convert::{proto_message_to_provider, provider_message_to_proto};
use hangar_proto::{ContentBlock, Message, ToolDefinition, TurnRequest, TurnState, TurnStateEvent};
use tokio::sync::Mutex;

use crate::agent::{self, LoopError, LoopHalt, LoopMode};
use crate::clients::{HangarClient, MainframeClient, TightbeamClient};
use crate::conversation::{sha256_hex, strip_frontmatter, AssistantAttribution, HistoryScope};
use crate::message_source::MessageSource;
use crate::registry::ConversationRegistry;
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn message_loop(
    max_iterations: u32,
    idle_gap: std::time::Duration,
    hangar: &mut HangarClient,
    tightbeam: &mut TightbeamClient,
    agent_cache: Arc<Mutex<String>>,
    tool_router: Arc<ToolRouter>,
    registry: Arc<ConversationRegistry>,
    message_source: &mut dyn MessageSource,
) -> Result<(), String> {
    // The transponder is the sole author of conversation history. Each
    // inbound message carries the conversation_id minted via
    // MintConversation; we load that conversation's log, append the user
    // turn, and resend the full assembled history every turn (providers
    // are stateless). Pod restart replays the on-disk log, so thread
    // context survives.
    loop {
        let inbound = message_source.next_message().await?;
        let tool_defs = tool_router.tool_definitions();

        // Read the primary persona from the cache. The agent watcher
        // populates this on startup (via initial_tx) and refreshes it
        // every 30s; a transient mainframe RPC failure between refreshes
        // leaves the previous good value in place.
        let persona = agent_cache.lock().await.clone();
        tracing::info!(bytes = persona.len(), "fetched primary persona");

        let conversation_id = inbound.conversation_id.clone();
        let reply_channel = inbound.reply_channel.clone();
        let conv_for_deliver = conversation_id.clone();
        let reply_for_deliver = reply_channel.clone();

        let log = registry
            .get_or_create(&conversation_id)
            .await
            .map_err(|e| format!("load conversation {conversation_id}: {e}"))?;
        registry.touch(&conversation_id).await;

        // Frontmatter carries model selection; the body is what the LLM
        // actually receives as its system prompt. The pre-strip persona is
        // hashed onto the assistant attribution for audit.
        let (system_body, frontmatter) = strip_frontmatter(&persona);
        let model = resolve_model(frontmatter.model.as_deref(), &log).await;

        // Append the user turn, then assemble the full provider history.
        let user_msg = Message {
            role: "user".into(),
            content: inbound.content,
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        };
        log.write()
            .await
            .append(proto_message_to_provider(&user_msg))
            .await
            .map_err(|e| format!("append user message: {e}"))?;
        let history: Vec<Message> = log
            .read()
            .await
            .history_for_provider(HistoryScope::Orchestrator)
            .iter()
            .map(provider_message_to_proto)
            .collect();

        let attribution = AssistantAttribution {
            model: model.clone(),
            system_prompt_sha256: Some(sha256_hex(&persona)),
            warnings: vec![],
        };

        let request = build_turn_request(
            Some(system_body),
            history,
            &tool_defs,
            model,
            reply_channel.clone(),
            conversation_id,
        );
        let result = agent::llm_loop(
            max_iterations,
            hangar,
            &*tool_router,
            &log,
            HistoryScope::Orchestrator,
            attribution,
            request,
            LoopMode {
                reply_channel,
                idle_gap,
            },
        )
        .await;
        // Transponder originates the client-facing reply + terminal turn-state
        // (the gateway set WORKING at ingest). hangar no longer delivers.
        deliver_turn_outcome(
            tightbeam,
            reply_for_deliver.as_deref(),
            &conv_for_deliver,
            &result,
        )
        .await;
        handle_llm_loop_result(result)?;
    }
}

/// Map an `llm_loop` outcome to the client-facing frame: the assistant reply
/// blocks (present on success / truncated-but-partial) and the terminal
/// turn-state (IDLE on a delivered reply, FAILED otherwise).
fn turn_outcome_frame(
    conversation_id: &str,
    result: &Result<String, LoopError>,
) -> (Option<Vec<ContentBlock>>, TurnStateEvent) {
    match result {
        Ok(text) | Err(LoopError::Halt(LoopHalt::MaxTokens(text))) => (
            Some(vec![agent::text_block(text.clone())]),
            TurnStateEvent {
                state: TurnState::Idle as i32,
                conversation_id: conversation_id.to_string(),
                ..Default::default()
            },
        ),
        Err(e) => (
            None,
            TurnStateEvent {
                state: TurnState::Failed as i32,
                conversation_id: conversation_id.to_string(),
                reason: loop_error_reason(e).to_string(),
                code: "13".into(),
            },
        ),
    }
}

fn loop_error_reason(e: &LoopError) -> &'static str {
    match e {
        LoopError::Halt(LoopHalt::IterationLimit { .. }) => "iteration limit reached",
        LoopError::Halt(LoopHalt::UnknownStop(_)) => "unexpected stop reason",
        LoopError::Halt(LoopHalt::MaxTokens(_)) => "response truncated",
        LoopError::HangarRpc(_) => "dispatch failed",
        LoopError::StreamEnded(_) => "turn stream ended",
        LoopError::ToolDispatch(_) => "tool dispatch failed",
    }
}

/// Push one orchestrator turn's outcome to the client via the gateway.
/// No-op when the turn had no reply channel; delivery failure is logged,
/// not fatal (the durable log already holds the reply).
async fn deliver_turn_outcome(
    tightbeam: &mut TightbeamClient,
    channel: Option<&str>,
    conversation_id: &str,
    result: &Result<String, LoopError>,
) {
    let Some(channel) = channel else { return };
    let (reply, turn_state) = turn_outcome_frame(conversation_id, result);
    if let Err(e) = tightbeam
        .deliver_outbound(channel, conversation_id, reply, Some(turn_state))
        .await
    {
        tracing::warn!(error = %e, "failed to deliver turn outcome to client");
    }
}

/// Resolve the dispatch model from frontmatter. `inherit` picks up the
/// model the previous in-scope assistant turn ran under; any other value
/// is taken literally. `None` (no frontmatter `model:`) returns `None`,
/// letting hangar fall back to its registered default.
///
// ponytail: inherit-from-default doesn't chain — when frontmatter omits
// `model`, the transponder doesn't know hangar's concrete default, so the
// assistant attribution records `None` and a later `inherit` falls back to
// the default again. Chaining a named model works; chaining the default
// would need hangar to echo the resolved model back on the turn stream.
async fn resolve_model(
    frontmatter_model: Option<&str>,
    log: &tokio::sync::RwLock<crate::conversation::ConversationLog>,
) -> Option<String> {
    match frontmatter_model {
        Some("inherit") => log
            .read()
            .await
            .last_assistant_model(HistoryScope::Orchestrator),
        Some(other) => Some(other.to_string()),
        None => None,
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
        // A turn that opened a stream and then ended or errored mid-turn
        // (worker reaped/crashed, a TurnError, or a close without Complete)
        // is a PER-TURN failure, not an infrastructure failure: log it and
        // await the next message instead of restarting the whole transponder
        // pod. A worker-reported error makes the controller broadcast FAILED
        // to the client; teardown/idle-gap cases are unblocked by reactive
        // teardown + the client's turn-state poll — no pod bounce needed.
        Err(LoopError::StreamEnded(e)) => {
            tracing::warn!(error = %e, "turn ended without completion, awaiting next user message");
            Ok(())
        }
        // `turn()` failing to OPEN (controller link down) or a reserved
        // tool-dispatch failure remain infrastructure failures → restart.
        Err(LoopError::HangarRpc(e)) | Err(LoopError::ToolDispatch(e)) => Err(e),
    }
}

/// Assemble a `TurnRequest`. `system` is already frontmatter-stripped and
/// `messages` is the full assembled history; the caller owns stripping,
/// model resolution, and history assembly.
fn build_turn_request(
    system: Option<String>,
    messages: Vec<Message>,
    tool_defs: &[ToolDefinition],
    model: Option<String>,
    reply_channel: Option<String>,
    conversation_id: String,
) -> TurnRequest {
    TurnRequest {
        system,
        tools: tool_defs.to_vec(),
        messages,
        model,
        reply_channel,
        role: None,
        correlation_id: None,
        conversation_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hangar_proto::{content_block, ContentBlock, TextBlock, ToolDefinition};

    fn user_text(s: &str) -> Vec<ContentBlock> {
        vec![ContentBlock {
            block: Some(content_block::Block::Text(TextBlock { text: s.into() })),
        }]
    }

    fn user_msg(s: &str) -> Vec<Message> {
        vec![Message {
            role: "user".into(),
            content: user_text(s),
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
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
            hangar_proto::StopReason::Unspecified,
        ))));
        assert!(res.is_ok());
    }

    #[test]
    fn handle_result_propagates_hangar_rpc() {
        let res = handle_llm_loop_result(Err(LoopError::HangarRpc("boom".into())));
        assert_eq!(res.unwrap_err(), "boom");
    }

    #[test]
    fn handle_result_swallows_stream_ended_without_restart() {
        // No-restart policy: a per-turn StreamEnded (worker reaped/crashed,
        // a TurnError, or a close without Complete) must NOT propagate — the
        // transponder logs it and awaits the next message instead of
        // restarting the pod. Mutant: revert StreamEnded to the restart
        // bucket → this returns Err and the test fails.
        let res = handle_llm_loop_result(Err(LoopError::StreamEnded("eof".into())));
        assert!(res.is_ok(), "a per-turn failure must not restart the pod");
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
            Some("PERSONA".into()),
            user_msg("hello"),
            &tool_defs,
            Some("claude-x".into()),
            Some("test-channel".into()),
            "test-conv".into(),
        );

        assert_eq!(req.system.as_deref(), Some("PERSONA"));
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "Bash");
        assert_eq!(req.role, None);
        assert_eq!(req.model.as_deref(), Some("claude-x"));
        assert_eq!(req.reply_channel.as_deref(), Some("test-channel"));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
    }

    #[test]
    fn stripped_system_lands_on_request_system() {
        let req = build_turn_request(
            Some("the persona text".into()),
            user_msg("hi"),
            &[],
            None,
            None,
            "conv".into(),
        );
        assert_eq!(req.system.as_deref(), Some("the persona text"));
    }

    #[test]
    fn turn_outcome_ok_delivers_reply_and_idle() {
        let (reply, ts) = turn_outcome_frame("c", &Ok("hi".into()));
        assert!(reply.is_some());
        assert_eq!(ts.state, TurnState::Idle as i32);
        assert_eq!(ts.conversation_id, "c");
    }

    #[test]
    fn turn_outcome_max_tokens_delivers_partial_reply_idle() {
        let (reply, ts) = turn_outcome_frame(
            "c",
            &Err(LoopError::Halt(LoopHalt::MaxTokens("partial".into()))),
        );
        assert!(reply.is_some());
        assert_eq!(ts.state, TurnState::Idle as i32);
    }

    #[test]
    fn turn_outcome_error_delivers_failed_no_reply() {
        let (reply, ts) = turn_outcome_frame("c", &Err(LoopError::HangarRpc("boom".into())));
        assert!(reply.is_none());
        assert_eq!(ts.state, TurnState::Failed as i32);
        assert!(!ts.reason.is_empty());
    }
}
