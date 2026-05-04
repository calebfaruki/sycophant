//! Entrypoint-driven runtime per decision 007.
//!
//! Per-user-message loop:
//! 1. Read `ENTRYPOINT.md` from the Mainframe mount (cached at startup).
//! 2. Construct a Tightbeam request with `system_prompt = entrypoint`,
//!    `messages = [user_message]`, `tools = full tool set`, `role = Agent`.
//! 3. Hand to `agent::tool_loop` for tool_use handling and channel emission.
//!
//! Recursion-blocking and delegate semantics are handled inside `tool_loop` via
//! the `llm_call` interception path (see `transponder_tools::dispatch_llm_call`).

use std::path::{Path, PathBuf};

use tightbeam_proto::{Message, TurnRequest};

use crate::agent;
use crate::clients::TightbeamClient;
use crate::message_source::MessageSource;
use crate::tool_router::ToolRouter;

const DEFAULT_ENTRYPOINT_PATH: &str = "/etc/mainframe/ENTRYPOINT.md";

pub(crate) async fn run(
    max_iterations: u32,
    tightbeam: &mut TightbeamClient,
    tool_router: &mut ToolRouter,
    message_source: &mut dyn MessageSource,
    entrypoint_path: Option<PathBuf>,
) -> Result<(), String> {
    let path = entrypoint_path.unwrap_or_else(|| PathBuf::from(DEFAULT_ENTRYPOINT_PATH));
    let entrypoint = load_entrypoint(&path)?;
    tracing::info!(
        path = %path.display(),
        bytes = entrypoint.len(),
        "loaded entrypoint"
    );

    let tool_defs = tool_router.tool_definitions();
    let mut first_turn = true;

    loop {
        let inbound = message_source.next_message().await?;
        let request = build_main_thread_request(
            &entrypoint,
            inbound.content,
            &tool_defs,
            &mut first_turn,
            inbound.reply_channel,
        );
        agent::tool_loop(max_iterations, tightbeam, tool_router, request).await?;
    }
}

fn load_entrypoint(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read entrypoint at {}: {e}", path.display()))
}

fn build_main_thread_request(
    entrypoint: &str,
    user_content: Vec<tightbeam_proto::ContentBlock>,
    tool_defs: &[tightbeam_proto::ToolDefinition],
    first_turn: &mut bool,
    reply_channel: Option<String>,
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
        let result = load_entrypoint(Path::new("/nonexistent/ENTRYPOINT.md"));
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("/nonexistent/ENTRYPOINT.md"), "got: {msg}");
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
            "ENTRYPOINT",
            user_text("hello"),
            &tool_defs,
            &mut first_turn,
            Some("test-channel".into()),
        );

        assert_eq!(req.system.as_deref(), Some("ENTRYPOINT"));
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
    fn subsequent_turns_omit_tools() {
        let tool_defs = vec![ToolDefinition {
            name: "bash".into(),
            description: "run shell".into(),
            parameters_json: "{}".into(),
        }];
        let mut first_turn = false;
        let req = build_main_thread_request(
            "ENTRYPOINT",
            user_text("again"),
            &tool_defs,
            &mut first_turn,
            None,
        );
        assert!(req.tools.is_empty());
    }
}
