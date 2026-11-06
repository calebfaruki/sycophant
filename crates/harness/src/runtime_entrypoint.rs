//! Per-user-message runtime loop.
//!
//! Each iteration:
//! 1. Read the primary agent (`AGENTS.md`) fresh from this agent's
//!    mounted instructions volume. A instructions edit takes effect on the next turn with
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
use crate::clients::{RelayClient, RelayRpc, ToolsetRpc};
use crate::conversation::{sha256_hex, strip_frontmatter, AssistantAttribution};
use crate::instructions::Instructions;
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
fn resolve_primary_agent(instructions: &Instructions, agent: &str) -> String {
    match instructions.read_primary_agent(agent) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "read primary agent failed, using empty system prompt");
            String::new()
        }
    }
}

/// Cap on the number of paths the primary prompt's tree map renders, matching
/// the `ListInstructions` runtime tool's cap.
const PRIMARY_TREE_CAP: usize = 1000;

/// Compose the model-facing primary system prompt: the AGENTS.md entry body
/// followed by a flat, capped map of the agent instruction tree's paths.
/// The map lets the primary turn see (and `ReadInstructions`) nested files it
/// would otherwise not know exist. An empty tree renders the body alone.
fn render_primary_prompt(agents_body: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return agents_body.to_string();
    }
    let mut prompt = agents_body.to_string();
    prompt.push_str("\n\n## Agent files\n\n");
    prompt.push_str(&paths.join("\n"));
    prompt
}

/// The per-turn inputs the primary turn sends: the system prompt (AGENTS.md
/// body with its frontmatter stripped, plus the tree map), the agent's one
/// provisioned model, and every advertised tool. Nothing is read from the
/// frontmatter.
fn turn_inputs(
    primary_agent_text: &str,
    tree: &[String],
    model: Option<String>,
    tool_router: &ToolRouter,
) -> (String, Option<String>, Vec<ToolDefinition>) {
    let body = strip_frontmatter(primary_agent_text);
    let system_prompt = render_primary_prompt(&body, tree);
    (system_prompt, model, tool_router.tool_definitions())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn message_loop(
    max_iterations: u32,
    idle_gap: std::time::Duration,
    model: Option<String>,
    toolset: &mut dyn ToolsetRpc,
    relay: &mut RelayClient,
    instructions: &Instructions,
    agent: &str,
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

        // Read the primary agent (`AGENTS.md`) fresh from the mounted instructions
        // volume for this turn. A instructions edit is picked up on the next turn
        // with no restart.
        let primary_agent_text = resolve_primary_agent(instructions, agent);
        tracing::info!(bytes = primary_agent_text.len(), "read primary agent");

        let conversation_id = inbound.conversation_id.clone();
        let reply_channel_stream = inbound.reply_channel_stream.clone();
        tracing::info!(conversation_id = %conversation_id, reply_channel_stream = ?reply_channel_stream, "inbound reply channel");
        let conv_for_deliver = conversation_id.clone();
        let reply_for_deliver = reply_channel_stream.clone();

        let log = registry
            .get_or_create(&conversation_id)
            .await
            .map_err(|e| format!("load conversation {conversation_id}: {e}"))?;
        registry.touch(&conversation_id).await;

        // The pre-strip agent text is hashed onto the assistant attribution
        // for audit. The model-facing prompt is the AGENTS.md body plus a
        // flat, capped map of the agent instruction tree, so the primary turn
        // can see and reach nested files.
        let (system_prompt, model, tool_defs) = turn_inputs(
            &primary_agent_text,
            &instructions.list_tree(agent, PRIMARY_TREE_CAP),
            model.clone(),
            &tool_router,
        );

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
        let history: Vec<Message> = log.read().await.history_for_provider();

        let prompt_hash = sha256_hex(&primary_agent_text);
        let attribution = AssistantAttribution {
            model: model.clone(),
            system_prompt_sha256: Some(prompt_hash.clone()),
            warnings: vec![],
        };

        // Turn-start identity frame: name + system-prompt hash so the client
        // can label the agent and warn on a prompt change between turns.
        // Empty agent_name = the primary agent, AGENTS.md (unnamed here).
        if reply_channel_stream.is_some() {
            let start = turn_start_frame(&conv_for_deliver, "", &prompt_hash);
            if let Err(e) = relay.deliver_turn_state(start, "").await {
                tracing::warn!(error = %e, "failed to deliver turn-start identity frame");
            }
        }

        // Per-turn cancellation token, fired by a client CancelTurn.
        let cancel = registry.register_turn(&conv_for_deliver).await;

        let request = build_turn_request(
            Some(system_prompt),
            history,
            &tool_defs,
            model,
            reply_channel_stream.clone(),
            conversation_id,
        );
        // Stream activity frames to the client only when there is a reply
        // channel stream (mirrors deliver_turn_outcome's early return). The sink
        // borrows `relay` for the loop's duration; scope it so the
        // borrow ends before the terminal delivery below reborrows it.
        let result = {
            let mut null_sink = crate::turn::NullSink;
            let mut gateway_sink;
            let sink: &mut dyn StreamSink = match reply_channel_stream.clone() {
                Some(channel_stream_id) => {
                    gateway_sink = crate::turn::GatewaySink {
                        rpc: &mut *relay,
                        channel_stream_id,
                    };
                    &mut gateway_sink
                }
                None => &mut null_sink,
            };
            agent::llm_loop(
                max_iterations,
                &mut *toolset,
                &*tool_router,
                &log,
                attribution,
                request,
                LoopMode {
                    reply_channel_stream: reply_channel_stream.clone(),
                    idle_gap,
                    cancel: cancel.clone(),
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
            &registry,
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
/// The terminal turn-state frame carries the conversation's owner as its
/// subject, so the relay records it under the key the gateway minted the
/// conversation with. No-op when the turn had no reply channel stream; delivery
/// failure is logged, not fatal (the durable log already holds the reply).
async fn deliver_turn_outcome(
    relay: &mut impl RelayRpc,
    registry: &ConversationRegistry,
    reply_channel_stream: Option<&str>,
    conversation_id: &str,
    result: &Result<String, LoopError>,
) {
    let Some(_) = reply_channel_stream else {
        tracing::warn!(conversation_id = %conversation_id, "no reply channel, dropping turn outcome");
        return;
    };
    let (reply, turn_state) = turn_outcome_frame(conversation_id, result);
    // Deliver reply first (if any), then turn-state — preserving ordering.
    if let Some(content) = reply {
        tracing::info!(conversation_id = %conversation_id, "delivering reply to relay");
        if let Err(e) = relay.deliver_reply(conversation_id, content).await {
            tracing::warn!(error = %e, "failed to deliver reply to client");
        } else {
            tracing::info!(conversation_id = %conversation_id, "reply delivered to relay");
        }
    }
    let subject = registry.owner_of(conversation_id).await.unwrap_or_default();
    tracing::info!(conversation_id = %conversation_id, "delivering turn state to relay");
    if let Err(e) = relay.deliver_turn_state(turn_state, &subject).await {
        tracing::warn!(error = %e, "failed to deliver turn state to client");
    } else {
        tracing::info!(conversation_id = %conversation_id, "turn state delivered to relay");
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
        // (inference job reaped/crashed, a TurnError, or a close without Complete)
        // is a PER-TURN failure, not an infrastructure failure: log it and
        // await the next message instead of restarting the whole harness
        // pod. An inference-job-reported error surfaces to the client as FAILED;
        // teardown/idle-gap cases are unblocked by reactive
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
    reply_channel_stream: Option<String>,
    conversation_id: String,
) -> TurnRequest {
    TurnRequest {
        system,
        tools: tool_defs.to_vec(),
        messages,
        model,
        reply_channel_stream,
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
        std::fs::create_dir(tmp.path().join("ag1")).unwrap();
        let instructions = Instructions::new(tmp.path());
        assert_eq!(resolve_primary_agent(&instructions, "ag1"), "");
    }

    // The primary turn injects a flat, capped path map from
    // `instructions.list_tree(agent, cap)` into the primary system prompt, keeping
    // AGENTS.md as the required entry prompt.
    #[test]
    fn primary_prompt_injects_flat_instructions_path_map_and_keeps_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("ag1/skills/foo")).unwrap();
        std::fs::create_dir_all(tmp.path().join("ag1/agents/team")).unwrap();
        std::fs::write(
            tmp.path().join("ag1/AGENTS.md"),
            "# Primary\n\nEntry prose.",
        )
        .unwrap();
        std::fs::write(tmp.path().join("ag1/skills/foo/SKILL.md"), "s").unwrap();
        std::fs::write(tmp.path().join("ag1/agents/team/scribe.md"), "a").unwrap();
        let instructions = Instructions::new(tmp.path());

        let agent_text = resolve_primary_agent(&instructions, "ag1");
        let body = strip_frontmatter(&agent_text);
        let prompt = render_primary_prompt(&body, &instructions.list_tree("ag1", PRIMARY_TREE_CAP));
        assert!(
            prompt.contains("Entry prose."),
            "AGENTS.md body is kept as the primary prompt: {prompt}"
        );
        assert!(
            prompt.contains("skills/foo/SKILL.md"),
            "the nested skill path is injected into the primary prompt: {prompt}"
        );
        assert!(
            prompt.contains("agents/team/scribe.md"),
            "the nested agent path is injected into the primary prompt: {prompt}"
        );
    }

    /// The agent's one provisioned model names every turn and every bound tool
    /// is advertised: the AGENTS.md frontmatter's `model` and `tools` control
    /// nothing, and the frontmatter is stripped from the system prompt. A build
    /// that still reads the frontmatter's `model` returns `other`; one that
    /// still scopes by its `tools` drops `Y`.
    #[test]
    fn turn_inputs_take_the_provisioned_model_and_every_tool_not_the_frontmatter() {
        use shared::toolset::{ToolDecl, ToolsetConfig, ToolsetEntry};
        use std::collections::HashMap;
        use std::sync::Arc;

        let root = tempfile::TempDir::new().unwrap().keep();
        std::fs::create_dir_all(root.join("ag")).unwrap();
        let registry_root = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn crate::conversation::ConversationStoreFactory> =
            Arc::new(crate::conversation::LocalFsFactory::new(registry_root));
        let router = crate::tool_router::ToolRouter::new(
            Arc::new(Instructions::new(root)),
            "ag".to_string(),
            None,
            Arc::new(crate::registry::ConversationRegistry::new(factory)),
        )
        .with_toolset_scope(vec!["ts".to_string()], HashMap::new());
        let tool = |name: &str| ToolDecl {
            name: name.into(),
            description: format!("desc:{name}"),
            args: Vec::new(),
        };
        let entry = ToolsetEntry {
            tools: vec![tool("X"), tool("Y")],
            ..Default::default()
        };
        let warnings = router.apply_toolsets(&ToolsetConfig::from_map(HashMap::from([(
            "ts".to_string(),
            entry,
        )])));
        assert!(warnings.is_empty(), "{warnings:?}");

        let (prompt, model, tools) = turn_inputs(
            "---\nmodel: other\ntools: [X]\n---\nbody",
            &[],
            Some("one".into()),
            &router,
        );

        assert_eq!(model.as_deref(), Some("one"));
        let names: Vec<String> = tools.into_iter().map(|d| d.name).collect();
        for want in ["X", "Y", "ReadInstructions"] {
            assert!(names.iter().any(|n| n == want), "{want} missing: {names:?}");
        }
        assert_eq!(prompt, "body");
        assert!(!prompt.contains("model:"), "{prompt}");
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
    fn handle_result_only_propagates_toolset_rpc_errors() {
        // Only a ToolsetRpc error propagates. No-restart policy: a per-turn
        // StreamEnded (inference job reaped/crashed, a TurnError, or a close
        // without Complete) must NOT propagate — the harness logs it and awaits
        // the next message instead of restarting the pod.
        let cases: [(&str, Result<String, LoopError>, Option<&str>); 6] = [
            (
                "iteration limit",
                Err(LoopError::Halt(LoopHalt::IterationLimit { limit: 7 })),
                None,
            ),
            (
                "max tokens with partial text",
                Err(LoopError::Halt(LoopHalt::MaxTokens("partial".into()))),
                None,
            ),
            (
                "unknown stop",
                Err(LoopError::Halt(LoopHalt::UnknownStop(
                    proto_common::StopReason::Unspecified,
                ))),
                None,
            ),
            (
                "toolset rpc",
                Err(LoopError::ToolsetRpc("boom".into())),
                Some("boom"),
            ),
            (
                "stream ended",
                Err(LoopError::StreamEnded("eof".into())),
                None,
            ),
            ("ok", Ok("final text".into()), None),
        ];
        for (case, result, want_err) in cases {
            let res = handle_llm_loop_result(result);
            assert_eq!(res.err().as_deref(), want_err, "{case}");
        }
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
        assert_eq!(req.model.as_deref(), Some("claude-x"));
        assert_eq!(req.reply_channel_stream.as_deref(), Some("test-channel"));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
    }

    #[test]
    fn turn_outcome_only_delivers_a_reply_for_ok_and_max_tokens() {
        // Ok and MaxTokens deliver a reply and land IDLE. The MaxTokens guard is
        // the only branch that splits IDLE from FAILED inside the error space.
        // Every other error — ToolsetRpc, every non-MaxTokens Halt, and
        // StreamEnded — must land FAILED with no reply, a reason, gRPC status
        // code 13 (INTERNAL) and the turn's conversation_id (so the client
        // reconciler routes the failure to the right conversation).
        let idle: [Result<String, LoopError>; 2] = [
            Ok("hi".into()),
            Err(LoopError::Halt(LoopHalt::MaxTokens("partial".into()))),
        ];
        for case in &idle {
            let (reply, ts) = turn_outcome_frame("c", case);
            assert!(reply.is_some(), "expected a reply for {case:?}");
            assert_eq!(
                ts.state,
                TurnState::Idle as i32,
                "expected Idle for {case:?}"
            );
            assert_eq!(ts.conversation_id, "c", "conversation_id for {case:?}");
        }

        let failed: [Result<String, LoopError>; 4] = [
            Err(LoopError::ToolsetRpc("boom".into())),
            Err(LoopError::Halt(LoopHalt::IterationLimit { limit: 7 })),
            Err(LoopError::Halt(LoopHalt::UnknownStop(
                proto_common::StopReason::Unspecified,
            ))),
            Err(LoopError::StreamEnded("eof".into())),
        ];
        for case in &failed {
            let (reply, ts) = turn_outcome_frame("c", case);
            assert!(reply.is_none(), "expected no reply for {case:?}");
            assert_eq!(
                ts.state,
                TurnState::Failed as i32,
                "expected Failed for {case:?}"
            );
            assert!(!ts.reason.is_empty(), "expected a reason for {case:?}");
            assert_eq!(ts.conversation_id, "c", "conversation_id for {case:?}");
            assert_eq!(ts.code, "13", "INTERNAL status code for {case:?}");
        }

        // A cancelled turn emits a terminal turn_cancelled event: the single
        // terminal funnel maps a Cancelled loop outcome to
        // TurnStateEvent{ state: CANCELLED }. CANCELLED is terminal but distinct
        // from FAILED (no error reason), so the client re-enables input without
        // an error banner.
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

    // The relay records a terminal state under the subject on the frame, the
    // same key the gateway minted the conversation under. An empty subject
    // records it nowhere, so polling clients would keep seeing WORKING.
    #[tokio::test]
    async fn deliver_turn_outcome_sends_the_owner_as_the_terminal_turn_state_subject() {
        use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
        let root = tempfile::TempDir::new().unwrap();
        let factory: Arc<dyn ConversationStoreFactory> =
            Arc::new(LocalFsFactory::new(root.path().to_path_buf()));
        let registry = Arc::new(ConversationRegistry::new(factory));
        let owner = "owner-stamp-alice";
        let conversation_id = registry.mint(owner).await.unwrap();

        let mut relay = crate::test_doubles::RecordingRelay::default();
        let result: Result<String, LoopError> = Ok("done".into());
        deliver_turn_outcome(
            &mut relay,
            &registry,
            Some("chan-1"),
            &conversation_id,
            &result,
        )
        .await;

        assert_eq!(
            relay.turn_states.len(),
            1,
            "one terminal turn-state frame is delivered"
        );
        let (turn_state, subject) = &relay.turn_states[0];
        assert_eq!(turn_state.state, TurnState::Idle as i32);
        assert_eq!(turn_state.conversation_id, conversation_id);
        assert_eq!(
            subject, owner,
            "the terminal turn state carries the conversation's owner stamp as its subject"
        );
    }
}
