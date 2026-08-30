//! Per-user-message runtime loop.
//!
//! Each iteration:
//! 1. Read the primary agent (`AGENTS.md`) fresh from this workspace's
//!    mounted kernel volume. A kernel edit takes effect on the next turn with
//!    no pod restart; a missing/unreadable file yields an empty system prompt
//!    for that turn rather than dropping the inbound message.
//! 2. Build a `TurnRequest` with `system = primary_agent_text`, the new user message,
//!    and the full tool set. Anthropic treats each request as stateless,
//!    so tools must be sent on every turn — `agent::llm_loop` propagates
//!    them through tool-result continuations and nudges.
//! 3. Hand to `agent::llm_loop`. Terminal-state policy (warn-and-continue
//!    on `Halt::*`, propagate on infra failures) is decided by
//!    `handle_llm_loop_result` at this layer.

use std::sync::Arc;

use proto_common::{ContentBlock, Message, ToolDefinition, TurnState, TurnStateEvent};
use toolset_proto::TurnRequest;

use crate::agent::{self, LoopError, LoopHalt, LoopMode};
use crate::clients::{RelayClient, ToolsetClient};
use crate::conversation::{sha256_hex, strip_frontmatter, AssistantAttribution, HistoryScope};
use crate::kernel::Kernel;
use crate::message_source::MessageSource;
use crate::registry::ConversationRegistry;
use crate::tool_router::ToolRouter;
use crate::turn::StreamSink;

/// Env var naming the harness's secret registry for streamed-item
/// scrubbing. Unset today — the harness holds no secrets, so the built
/// `ScrubSet` is empty and `scrub_frame` is a no-op. Wired so redaction is
/// mechanically in place once the harness is provisioned with a registry.
const SCRUB_REGISTRY_ENV: &str = "HARNESS_SCRUB_SECRETS";

/// Read the primary agent (`AGENTS.md`) for this turn. A missing or
/// unreadable file yields an empty system prompt rather than dropping the
/// inbound message: the turn still runs, just without an agent.
fn resolve_primary_agent(kernel: &Kernel, workspace: &str) -> String {
    match kernel.read_primary_agent(workspace) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "read primary agent failed, using empty system prompt");
            String::new()
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn message_loop(
    max_iterations: u32,
    idle_gap: std::time::Duration,
    toolset: &mut ToolsetClient,
    relay: &mut RelayClient,
    kernel: &Kernel,
    workspace: &str,
    tool_router: Arc<ToolRouter>,
    registry: Arc<ConversationRegistry>,
    message_source: &mut dyn MessageSource,
) -> Result<(), String> {
    // The harness is the sole author of conversation history. Each
    // inbound message carries the conversation_id minted via
    // MintConversation; we load that conversation's log, append the user
    // turn, and resend the full assembled history every turn (providers
    // are stateless). Pod restart replays the on-disk log, so thread
    // context survives.
    let scrub = shared::scrub::ScrubSet::from_env_var(SCRUB_REGISTRY_ENV);
    loop {
        let inbound = message_source.next_message().await?;

        // Read the primary agent (`AGENTS.md`) fresh from the mounted kernel
        // volume for this turn. A kernel edit is picked up on the next turn
        // with no restart.
        let primary_agent_text = resolve_primary_agent(kernel, workspace);
        tracing::info!(bytes = primary_agent_text.len(), "read primary agent");

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
        // actually receives as its system prompt. The pre-strip agent text is
        // hashed onto the assistant attribution for audit.
        let (system_body, frontmatter) = strip_frontmatter(&primary_agent_text);
        let model = resolve_model(frontmatter.model.as_deref(), Some(&log)).await;
        let tool_defs = tool_router.tool_definitions_scoped(frontmatter.tools.as_deref());

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
            .append(user_msg)
            .await
            .map_err(|e| format!("append user message: {e}"))?;
        let history: Vec<Message> = log
            .read()
            .await
            .history_for_provider(HistoryScope::Orchestrator);

        let prompt_hash = sha256_hex(&primary_agent_text);
        let attribution = AssistantAttribution {
            model: model.clone(),
            system_prompt_sha256: Some(prompt_hash.clone()),
            warnings: vec![],
        };

        // Turn-start identity frame: name + system-prompt hash so the client
        // can label the agent and warn on a prompt change between turns.
        // Empty agent_name = the workspace primary agent (unnamed here).
        if let Some(channel) = reply_channel.as_deref() {
            let start = turn_start_frame(&conv_for_deliver, "", &prompt_hash);
            if let Err(e) = relay
                .deliver_outbound(channel, &conv_for_deliver, None, Some(start))
                .await
            {
                tracing::warn!(error = %e, "failed to deliver turn-start identity frame");
            }
        }

        // Per-turn cancellation token, fired by a client CancelTurn.
        let cancel = registry.register_turn(&conv_for_deliver).await;

        let request = build_turn_request(
            Some(system_body),
            history,
            &tool_defs,
            model,
            reply_channel.clone(),
            conversation_id,
        );
        // Stream activity frames to the client only when there is a reply
        // channel (mirrors deliver_turn_outcome's early return). The sink
        // borrows `relay` for the loop's duration; scope it so the
        // borrow ends before the terminal delivery below reborrows it.
        let result = {
            let mut null_sink = crate::turn::NullSink;
            let mut gateway_sink;
            let sink: &mut dyn StreamSink = match reply_channel.clone() {
                Some(channel_id) => {
                    gateway_sink = crate::turn::GatewaySink {
                        rpc: &mut *relay,
                        channel_id,
                    };
                    &mut gateway_sink
                }
                None => &mut null_sink,
            };
            agent::llm_loop(
                max_iterations,
                toolset,
                &*tool_router,
                &log,
                HistoryScope::Orchestrator,
                attribution,
                request,
                LoopMode {
                    reply_channel: reply_channel.clone(),
                    idle_gap,
                    cancel: cancel.clone(),
                    grants: inbound.grants,
                },
                sink,
                &scrub,
            )
            .await
        };
        registry.end_turn(&conv_for_deliver).await;
        // Harness originates the client-facing reply + terminal turn-state
        // (the gateway set WORKING at ingest). the toolset controller no longer delivers.
        deliver_turn_outcome(
            relay,
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
        // Client-initiated local stop: terminal but not an error — no reply,
        // no failure reason, so the client re-enables input without a banner.
        Err(LoopError::Cancelled) => (
            None,
            TurnStateEvent {
                state: TurnState::Cancelled as i32,
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
                ..Default::default()
            },
        ),
    }
}

fn loop_error_reason(e: &LoopError) -> &'static str {
    match e {
        LoopError::Halt(LoopHalt::IterationLimit { .. }) => "iteration limit reached",
        LoopError::Halt(LoopHalt::UnknownStop(_)) => "unexpected stop reason",
        LoopError::Halt(LoopHalt::MaxTokens(_)) => "response truncated",
        LoopError::ToolsetRpc(_) => "dispatch failed",
        LoopError::StreamEnded(_) => "turn stream ended",
        LoopError::ToolDispatch(_) => "tool dispatch failed",
        LoopError::Cancelled => "turn cancelled",
    }
}

/// Build the harness-emitted turn-start frame carrying agent identity.
/// WORKING is idempotent for the client's push-authoritative reconciler; the
/// identity fields drive the client's label and prompt-change warning.
fn turn_start_frame(
    conversation_id: &str,
    agent_name: &str,
    system_prompt_sha256: &str,
) -> TurnStateEvent {
    TurnStateEvent {
        state: TurnState::Working as i32,
        conversation_id: conversation_id.to_string(),
        agent_name: agent_name.to_string(),
        system_prompt_sha256: system_prompt_sha256.to_string(),
        ..Default::default()
    }
}

/// Push one orchestrator turn's outcome to the client via the gateway.
/// No-op when the turn had no reply channel; delivery failure is logged,
/// not fatal (the durable log already holds the reply).
async fn deliver_turn_outcome(
    relay: &mut RelayClient,
    channel: Option<&str>,
    conversation_id: &str,
    result: &Result<String, LoopError>,
) {
    let Some(channel) = channel else { return };
    let (reply, turn_state) = turn_outcome_frame(conversation_id, result);
    if let Err(e) = relay
        .deliver_outbound(channel, conversation_id, reply, Some(turn_state))
        .await
    {
        tracing::warn!(error = %e, "failed to deliver turn outcome to client");
    }
}

/// Resolve the dispatch model from frontmatter. `inherit` picks up the
/// model the previous in-scope assistant turn ran under, and resolves to
/// nothing without a log to read. Any other value is taken literally. `None`
/// (no frontmatter `model:`) returns `None`, which the toolset controller
/// refuses. There is no default.
pub(crate) async fn resolve_model(
    frontmatter_model: Option<&str>,
    log: Option<&tokio::sync::RwLock<crate::conversation::ConversationLog>>,
) -> Option<String> {
    match frontmatter_model {
        Some("inherit") => log?
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
        // (prompt job reaped/crashed, a TurnError, or a close without Complete)
        // is a PER-TURN failure, not an infrastructure failure: log it and
        // await the next message instead of restarting the whole harness
        // pod. A prompt-job-reported error makes the controller broadcast FAILED
        // to the client; teardown/idle-gap cases are unblocked by reactive
        // teardown + the client's turn-state poll — no pod bounce needed.
        Err(LoopError::StreamEnded(e)) => {
            tracing::warn!(error = %e, "turn ended without completion, awaiting next user message");
            Ok(())
        }
        // A client cancel is a clean per-turn stop, not an infra failure.
        Err(LoopError::Cancelled) => {
            tracing::info!("turn cancelled by client, awaiting next user message");
            Ok(())
        }
        // `turn()` failing to OPEN (controller link down) or a reserved
        // tool-dispatch failure remain infrastructure failures → restart.
        Err(LoopError::ToolsetRpc(e)) | Err(LoopError::ToolDispatch(e)) => Err(e),
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
    use proto_common::{content_block, ContentBlock, TextBlock, ToolDefinition};

    #[test]
    fn resolve_primary_agent_missing_file_is_empty_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("ws1")).unwrap();
        let kernel = Kernel::new(tmp.path());
        assert_eq!(resolve_primary_agent(&kernel, "ws1"), "");
    }

    #[test]
    fn resolve_primary_agent_present_file_is_served_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("ws1")).unwrap();
        std::fs::write(tmp.path().join("ws1/AGENTS.md"), "# Agent\n\nHello.").unwrap();
        let kernel = Kernel::new(tmp.path());
        assert_eq!(resolve_primary_agent(&kernel, "ws1"), "# Agent\n\nHello.");
    }

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
            proto_common::StopReason::Unspecified,
        ))));
        assert!(res.is_ok());
    }

    #[test]
    fn handle_result_propagates_toolset_rpc() {
        let res = handle_llm_loop_result(Err(LoopError::ToolsetRpc("boom".into())));
        assert_eq!(res.unwrap_err(), "boom");
    }

    #[test]
    fn handle_result_swallows_stream_ended_without_restart() {
        // No-restart policy: a per-turn StreamEnded (prompt job reaped/crashed,
        // a TurnError, or a close without Complete) must NOT propagate — the
        // harness logs it and awaits the next message instead of
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
            Some("AGENT".into()),
            user_msg("hello"),
            &tool_defs,
            Some("claude-x".into()),
            Some("test-channel".into()),
            "test-conv".into(),
        );

        assert_eq!(req.system.as_deref(), Some("AGENT"));
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
            Some("the agent text".into()),
            user_msg("hi"),
            &[],
            None,
            None,
            "conv".into(),
        );
        assert_eq!(req.system.as_deref(), Some("the agent text"));
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
        let (reply, ts) = turn_outcome_frame("c", &Err(LoopError::ToolsetRpc("boom".into())));
        assert!(reply.is_none());
        assert_eq!(ts.state, TurnState::Failed as i32);
        assert!(!ts.reason.is_empty());
        // The failed frame must carry the turn's conversation_id so the client
        // reconciler routes the failure to the right conversation. Mutant:
        // drop conversation_id from the Err arm → this is empty and fails.
        assert_eq!(ts.conversation_id, "c");
        // gRPC status code 13 (INTERNAL) accompanies every failed turn. Mutant:
        // drop `code: "13"` → this is empty and fails.
        assert_eq!(ts.code, "13");
    }

    #[test]
    fn turn_outcome_non_maxtokens_halt_and_stream_ended_all_fail() {
        // The MaxTokens guard is the only branch that splits IDLE from FAILED
        // inside the error space. Every other error — every non-MaxTokens Halt
        // plus StreamEnded — must land FAILED with no reply. Existing coverage
        // only exercises ToolsetRpc. Mutant: widen the MaxTokens arm to swallow
        // another variant → that variant delivers IDLE + a reply and this fails.
        let cases: [Result<String, LoopError>; 3] = [
            Err(LoopError::Halt(LoopHalt::IterationLimit { limit: 7 })),
            Err(LoopError::Halt(LoopHalt::UnknownStop(
                proto_common::StopReason::Unspecified,
            ))),
            Err(LoopError::StreamEnded("eof".into())),
        ];
        for case in &cases {
            let (reply, ts) = turn_outcome_frame("c", case);
            assert!(reply.is_none(), "expected no reply for {case:?}");
            assert_eq!(
                ts.state,
                TurnState::Failed as i32,
                "expected Failed for {case:?}"
            );
            assert!(!ts.reason.is_empty(), "expected a reason for {case:?}");
        }
    }

    // A cancelled turn emits a terminal turn_cancelled event: the single
    // terminal funnel maps a Cancelled loop outcome to
    // TurnStateEvent{ state: CANCELLED }. CANCELLED is terminal but distinct
    // from FAILED (no error reason), so the client re-enables input without an
    // error banner.
    #[test]
    fn turn_outcome_cancelled_emits_terminal_cancelled_state() {
        // Materiality: map the Cancelled outcome to IDLE or FAILED instead of
        // CANCELLED (flip the match arm) -> the client cannot distinguish a
        // clean cancel from a normal finish or an error.
        let (reply, ts) = turn_outcome_frame("c", &Err(LoopError::Cancelled));
        assert_eq!(
            ts.state,
            TurnState::Cancelled as i32,
            "cancel is its own terminal state, not IDLE/FAILED"
        );
        assert_eq!(ts.conversation_id, "c");
        // Not an error: a cancel must NOT carry a failure reason banner.
        assert!(
            ts.reason.is_empty(),
            "cancel is terminal-but-not-error; no failure reason"
        );
        // No assistant reply is delivered for an abandoned turn.
        assert!(reply.is_none());
    }

    // EARS: "When a turn starts, the client shall display the agent identity
    // (name when present)" and "Where a turn's system_prompt_sha256 differs
    // from the prior turn's ... surface a prompt-change warning." Both are
    // driven by a harness-emitted turn-start frame carrying identity
    // (plan 0b/3a). This pins that the turn-start frame carries name + hash;
    // the client-side label/warning are tested in Dart.
    #[test]
    fn turn_start_frame_carries_identity() {
        // Materiality: drop agent_name / system_prompt_sha256 from the
        // turn-start builder (or emit a bare WORKING without them) -> the
        // client never sees identity and can neither label nor diff the hash.
        let frame = turn_start_frame("conv-9", "helper-agent", "abc123deadbeef");
        assert_eq!(frame.state, TurnState::Working as i32);
        assert_eq!(frame.conversation_id, "conv-9");
        assert_eq!(frame.agent_name, "helper-agent");
        assert_eq!(frame.system_prompt_sha256, "abc123deadbeef");
    }
}
