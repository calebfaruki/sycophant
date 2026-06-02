//! Entrypoint-driven runtime per decision 007.
//!
//! Per-user-message loop:
//! 1. Re-read `AGENTS.md` from the Mainframe mount on every turn so principal
//!    edits to the host filesystem (per ADR 010 HostPath model) take effect
//!    without a pod restart.
//! 2. Construct a Tightbeam request with `system_prompt = entrypoint`,
//!    `messages = [user_message]`, `tools = full tool set`, `role = Agent`.
//! 3. Hand to `agent::tool_loop` for tool_use handling and channel emission.
//!
//! Recursion-blocking and delegate semantics are handled inside `tool_loop` via
//! the `llm_call` interception path (see `transponder_tools::dispatch_llm_call`).

use std::path::Path;
use std::sync::Arc;

use tightbeam_proto::{Message, ToolDefinition, TurnRequest};
use tokio::sync::Mutex;

use crate::agent;
use crate::clients::TightbeamClient;
use crate::message_source::MessageSource;
use crate::tool_router::ToolRouter;
use crate::transponder_tools;

const ENTRYPOINT_PATH: &str = "/etc/kernel/AGENTS.md";

pub(crate) async fn run(
    max_iterations: u32,
    tightbeam: &mut TightbeamClient,
    tool_router: Arc<Mutex<ToolRouter>>,
    message_source: &mut dyn MessageSource,
) -> Result<(), String> {
    // Mint a conversation id once per process lifetime. Every user message
    // this pod sees joins the same conversation thread. Pod restart =
    // fresh conversation; that's acceptable for V0 since the SaaS Rails
    // app isn't shipping per-session conversation routing yet.
    let conversation_id = tightbeam.mint_conversation().await?;
    tracing::info!(
        conversation_id = %conversation_id,
        "minted conversation id for transponder lifetime"
    );

    loop {
        let inbound = message_source.next_message().await?;
        // Lock the router for the duration of this user message. The
        // background `watch_airlock_tools` task waits at most one
        // user-message-cycle before applying any pushed snapshot — acceptable
        // since chamber changes are rare.
        let mut router_guard = tool_router.lock().await;

        // Tool list advertised to the LLM = router-served tools (airlock,
        // current as of the lock acquisition) plus transponder
        // built-ins (e.g., llm_call). Recomputed per turn so chamber tool
        // updates pushed by the watch task surface to the LLM on the next
        // user message without a pod restart.
        let mut tool_defs = router_guard.tool_definitions();
        tool_defs.extend(transponder_tools::tool_definitions().into_iter().map(|t| {
            ToolDefinition {
                name: t.name,
                description: t.description,
                parameters_json: t.parameters_json,
            }
        }));

        // Reset per user message so each new exchange surfaces the current
        // tool set to the LLM. Pod-lifetime scoping was the original Issue 3
        // bug (tools sent only on the very first message after pod start).
        let mut first_turn = true;
        let request = build_turn_request_from_disk(
            Path::new(ENTRYPOINT_PATH),
            inbound.content,
            &tool_defs,
            &mut first_turn,
            inbound.reply_channel,
            conversation_id.clone(),
        )?;
        agent::tool_loop(max_iterations, tightbeam, &mut router_guard, request).await?;
    }
}

fn load_entrypoint(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read entrypoint at {}: {e}", path.display()))
}

fn build_turn_request_from_disk(
    path: &Path,
    user_content: Vec<tightbeam_proto::ContentBlock>,
    tool_defs: &[tightbeam_proto::ToolDefinition],
    first_turn: &mut bool,
    reply_channel: Option<String>,
    conversation_id: String,
) -> Result<TurnRequest, String> {
    let entrypoint = load_entrypoint(path)?;
    tracing::info!(
        path = %path.display(),
        bytes = entrypoint.len(),
        "read entrypoint"
    );
    Ok(build_main_thread_request(
        &entrypoint,
        user_content,
        tool_defs,
        first_turn,
        reply_channel,
        conversation_id,
    ))
}

fn build_main_thread_request(
    entrypoint: &str,
    user_content: Vec<tightbeam_proto::ContentBlock>,
    tool_defs: &[tightbeam_proto::ToolDefinition],
    first_turn: &mut bool,
    reply_channel: Option<String>,
    conversation_id: String,
) -> TurnRequest {
    let tools = if *first_turn {
        *first_turn = false;
        tool_defs.to_vec()
    } else {
        vec![]
    };

    TurnRequest {
        system: Some(entrypoint.to_string()),
        tools,
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
    use std::io::Write;
    use tempfile::NamedTempFile;
    use tightbeam_proto::{content_block, ContentBlock, TextBlock, ToolDefinition};

    fn user_text(s: &str) -> Vec<ContentBlock> {
        vec![ContentBlock {
            block: Some(content_block::Block::Text(TextBlock { text: s.into() })),
        }]
    }

    #[test]
    fn load_entrypoint_reads_file_contents() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "You are a helpful assistant.").unwrap();
        let contents = load_entrypoint(tmp.path()).unwrap();
        assert!(contents.contains("helpful assistant"));
    }

    #[test]
    fn load_entrypoint_errors_on_missing_file() {
        let result = load_entrypoint(Path::new("/nonexistent/AGENTS.md"));
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("/nonexistent/AGENTS.md"), "got: {msg}");
    }

    #[test]
    fn first_turn_carries_full_tool_set() {
        let tool_defs = vec![ToolDefinition {
            name: "bash".into(),
            description: "run shell".into(),
            parameters_json: "{}".into(),
        }];
        let mut first_turn = true;
        let req = build_main_thread_request(
            "AGENTS",
            user_text("hello"),
            &tool_defs,
            &mut first_turn,
            Some("test-channel".into()),
            "test-conv".into(),
        );

        assert_eq!(req.system.as_deref(), Some("AGENTS"));
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "bash");
        assert_eq!(req.role, None, "orchestrator turns leave role unset");
        assert_eq!(req.reply_channel.as_deref(), Some("test-channel"));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert!(
            !first_turn,
            "first_turn must flip to false after consumption"
        );
    }

    #[test]
    fn first_turn_false_omits_tools() {
        let tool_defs = vec![ToolDefinition {
            name: "bash".into(),
            description: "run shell".into(),
            parameters_json: "{}".into(),
        }];
        let mut first_turn = false;
        let req = build_main_thread_request(
            "AGENTS",
            user_text("again"),
            &tool_defs,
            &mut first_turn,
            None,
            "test-conv".into(),
        );
        assert!(req.tools.is_empty());
    }

    /// Per-message scoping: each new user message must reset `first_turn` so
    /// the LLM sees the current tool set on the first turn of every exchange.
    /// Pin this so a regression to pod-lifetime scoping (the original Issue 3
    /// bug) gets caught at the unit level.
    #[test]
    fn each_new_message_resets_first_turn_and_carries_tools() {
        let tool_defs = vec![ToolDefinition {
            name: "bash".into(),
            description: "run shell".into(),
            parameters_json: "{}".into(),
        }];

        let mut first_turn = true;
        let req1 = build_main_thread_request(
            "AGENTS",
            user_text("hello"),
            &tool_defs,
            &mut first_turn,
            None,
            "test-conv".into(),
        );
        assert_eq!(req1.tools.len(), 1, "first message must carry tools");

        // Caller (run loop) re-initializes the flag for the next user message.
        let mut first_turn = true;
        let req2 = build_main_thread_request(
            "AGENTS",
            user_text("hi again"),
            &tool_defs,
            &mut first_turn,
            None,
            "test-conv".into(),
        );
        assert_eq!(
            req2.tools.len(),
            1,
            "second user message must also carry tools (per-message scoping, not per-pod)"
        );
    }

    /// Per ADR 010: the entrypoint is re-read on every turn, so principal
    /// edits to the host filesystem land in the agent's system prompt without
    /// a pod restart. Pin this so the helper can't regress to startup-time
    /// caching unnoticed.
    #[test]
    fn per_turn_reread_picks_up_file_changes() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::fs::write(&path, "first\n").unwrap();

        let tool_defs: Vec<ToolDefinition> = vec![];
        let mut first_turn = true;

        let req1 = build_turn_request_from_disk(
            &path,
            user_text("hi"),
            &tool_defs,
            &mut first_turn,
            None,
            "test-conv".into(),
        )
        .unwrap();
        assert!(req1.system.as_deref().unwrap().contains("first"));

        // Overwrite between turns; the next call must reflect the new contents.
        std::fs::write(&path, "second\n").unwrap();

        let req2 = build_turn_request_from_disk(
            &path,
            user_text("hi again"),
            &tool_defs,
            &mut first_turn,
            None,
            "test-conv".into(),
        )
        .unwrap();
        assert!(req2.system.as_deref().unwrap().contains("second"));
        assert!(
            !req2.system.as_deref().unwrap().contains("first"),
            "second turn must not carry stale entrypoint contents"
        );
    }

    #[test]
    fn build_turn_request_from_disk_propagates_io_errors() {
        let tool_defs: Vec<ToolDefinition> = vec![];
        let mut first_turn = true;
        let result = build_turn_request_from_disk(
            Path::new("/nonexistent/AGENTS.md"),
            user_text("hi"),
            &tool_defs,
            &mut first_turn,
            None,
            "test-conv".into(),
        );
        assert!(result.is_err());
    }
}
