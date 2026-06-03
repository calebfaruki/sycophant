use airlock_proto::ToolInfo;
use serde::{Deserialize, Serialize};
use tightbeam_proto::{HistoryEntry, Message, TurnRequest, TurnRole};

use crate::agent::{self, text_block, LoopError, LoopHalt, LoopMode};
use crate::clients::TightbeamRpc;
use crate::tool_router::ToolDispatcher;

pub(crate) const LLM_CALL_TOOL_NAME: &str = "llm_call";
pub(crate) const RECENT_TURNS_TOOL_NAME: &str = "recent_turns";

#[derive(Deserialize)]
struct LlmCallArgs {
    system_prompt: String,
    query: String,
}

pub(crate) fn tool_definitions() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: LLM_CALL_TOOL_NAME.into(),
            description: "Dispatch a stateless sub-LLM call with a custom system prompt. \
                          Use this to delegate work to a different persona or specialist. \
                          The delegate has read access to the same files but no conversation history. \
                          Returns the delegate's final response as text."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "system_prompt": {
                        "type": "string",
                        "description": "System prompt for the delegate. The orchestrator typically reads an agent file from the Mainframe mount and passes its contents here."
                    },
                    "query": {
                        "type": "string",
                        "description": "The user-message-shaped query to send to the delegate. Construct whatever context the delegate needs into this field; the delegate will not see prior conversation history."
                    }
                },
                "required": ["system_prompt", "query"]
            })
            .to_string(),
        },
        ToolInfo {
            name: RECENT_TURNS_TOOL_NAME.into(),
            description: "Return the recent turns from the current conversation as JSON. \
                          Useful for reflecting on prior context that may have fallen out \
                          of the active prompt window, or when answering questions about \
                          what was previously discussed. Returns up to `limit` most recent \
                          entries (default 50, server-clamped to 500). Each entry includes \
                          its sequence number, timestamp, message body, and tag (if any)."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 500,
                        "description": "Maximum number of recent turns to return. Defaults to 50 when omitted."
                    }
                }
            })
            .to_string(),
        },
    ]
}

/// Dispatch an `llm_call` tool invocation. Spawns a delegate Tightbeam call with
/// `role = TurnRole::Delegate`, runs a tool loop for the delegate (with `llm_call`
/// excluded from the delegate's tool list — recursion is structurally blocked),
/// and returns the delegate's final assistant text.
pub(crate) async fn dispatch_llm_call(
    tightbeam: &mut dyn TightbeamRpc,
    tool_router: &mut dyn ToolDispatcher,
    correlation_id: &str,
    input_json: &str,
    max_iterations: u32,
    depth: u8,
) -> Result<String, String> {
    let args: LlmCallArgs =
        serde_json::from_str(input_json).map_err(|e| format!("invalid llm_call arguments: {e}"))?;

    // Delegate inherits only the router-served tools (mainframe + airlock).
    // llm_call is a transponder built-in advertised at the orchestrator's call
    // site, never in the router — so the delegate naturally cannot invoke it.
    // Recursion blocking is structural at the router-vs-builtins boundary.
    let delegate_tools = tool_router.tool_definitions();

    // Each delegate call is its own conversation thread — sub-conversations
    // are separate from the orchestrator's thread so the delegate's history
    // doesn't pollute the parent context.
    let delegate_conversation_id = tightbeam.mint_conversation().await?;
    let initial_request = TurnRequest {
        system: Some(args.system_prompt),
        tools: delegate_tools,
        messages: vec![Message {
            role: "user".into(),
            content: vec![text_block(args.query)],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        }],
        model: None,
        reply_channel: None,
        role: Some(TurnRole::Delegate as i32),
        correlation_id: Some(correlation_id.to_string()),
        conversation_id: delegate_conversation_id,
    };

    // Box::pin breaks the recursive async-fn cycle: llm_loop → dispatch_llm_call →
    // llm_loop. Without the box, the future would be infinitely sized.
    match Box::pin(agent::llm_loop(
        max_iterations,
        tightbeam,
        tool_router,
        initial_request,
        LoopMode::Delegate {
            correlation_id: correlation_id.to_string(),
        },
        depth,
    ))
    .await
    {
        Ok(text) => Ok(text),
        // MaxTokens preserves partial assistant text (best practice — matches
        // today's `EndTurn | MaxTokens => Ok(collect_text(...))` branch).
        Err(LoopError::Halt(LoopHalt::MaxTokens(text))) => Ok(text),
        Err(LoopError::Halt(LoopHalt::IterationLimit { limit })) => {
            Err(format!("delegate iteration limit ({limit}) reached"))
        }
        Err(LoopError::Halt(LoopHalt::UnknownStop(reason))) => {
            Err(format!("unexpected delegate stop reason: {reason:?}"))
        }
        Err(LoopError::ForbiddenInDelegate(tool)) => {
            Err(format!("delegate attempted forbidden tool: {tool}"))
        }
        Err(LoopError::TightbeamRpc(e))
        | Err(LoopError::ToolDispatch(e))
        | Err(LoopError::StreamEnded(e)) => Err(e),
    }
}

#[derive(Deserialize)]
struct RecentTurnsArgs {
    #[serde(default)]
    limit: Option<u32>,
}

/// Compact LLM-facing projection of a `HistoryEntry`. ContentBlocks
/// collapse to a single text field (their text segments joined); tool
/// calls and error markers travel as boolean/array fields. The agent
/// sees a stable JSON shape it can grep / parse without proto schema
/// knowledge.
#[derive(Serialize)]
struct RecentTurnEntry<'a> {
    seq: u64,
    ts: &'a str,
    role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ProjectedToolCall<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<&'a str>,
}

#[derive(Serialize)]
struct ProjectedToolCall<'a> {
    name: &'a str,
    input_json: &'a str,
}

fn project_history_entries(entries: &[HistoryEntry]) -> Vec<RecentTurnEntry<'_>> {
    entries
        .iter()
        .filter_map(|e| {
            e.message.as_ref().map(|m| RecentTurnEntry {
                seq: e.seq,
                ts: &e.ts,
                role: &m.role,
                text: Some(agent::collect_text(&m.content)).filter(|s| !s.is_empty()),
                tool_calls: m
                    .tool_calls
                    .iter()
                    .map(|tc| ProjectedToolCall {
                        name: &tc.name,
                        input_json: &tc.input_json,
                    })
                    .collect(),
                tool_call_id: m.tool_call_id.as_deref(),
                is_error: m.is_error,
                tag: e.tag.as_deref(),
            })
        })
        .collect()
}

/// Dispatch a `recent_turns` tool invocation. Calls
/// `GetConversationHistory` on tightbeam and serializes the response as
/// JSON for the LLM. `conversation_id` is threaded from the orchestrator
/// turn loop (each turn knows which conversation it's running for).
pub(crate) async fn dispatch_recent_turns(
    tightbeam: &mut dyn TightbeamRpc,
    conversation_id: &str,
    input_json: &str,
) -> Result<String, String> {
    let args: RecentTurnsArgs =
        serde_json::from_str(input_json).map_err(|e| format!("invalid recent_turns args: {e}"))?;
    let resp = tightbeam
        .get_conversation_history(conversation_id, args.limit)
        .await?;
    let projected = project_history_entries(&resp.entries);
    serde_json::to_string(&projected).map_err(|e| format!("recent_turns serialize: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_includes_llm_call_and_recent_turns() {
        let defs = tool_definitions();
        assert_eq!(defs.len(), 2);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&LLM_CALL_TOOL_NAME));
        assert!(names.contains(&RECENT_TURNS_TOOL_NAME));
    }

    #[test]
    fn llm_call_schema_requires_system_prompt_and_query() {
        let defs = tool_definitions();
        let llm = defs.iter().find(|d| d.name == LLM_CALL_TOOL_NAME).unwrap();
        let schema: serde_json::Value = serde_json::from_str(&llm.parameters_json).unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["system_prompt"].is_object());
        assert!(schema["properties"]["query"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "system_prompt"));
        assert!(required.iter().any(|v| v == "query"));
    }

    #[test]
    fn recent_turns_schema_has_optional_limit() {
        let defs = tool_definitions();
        let rt = defs
            .iter()
            .find(|d| d.name == RECENT_TURNS_TOOL_NAME)
            .unwrap();
        let schema: serde_json::Value = serde_json::from_str(&rt.parameters_json).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["limit"]["type"], "integer");
        // limit is intentionally not in `required` — agents can call with {}.
        assert!(
            schema.get("required").is_none() || schema["required"].as_array().unwrap().is_empty()
        );
    }

    #[test]
    fn recent_turns_args_accepts_empty_object() {
        let args: RecentTurnsArgs = serde_json::from_str("{}").unwrap();
        assert!(args.limit.is_none());
    }

    #[test]
    fn recent_turns_args_accepts_limit() {
        let args: RecentTurnsArgs = serde_json::from_str(r#"{"limit": 10}"#).unwrap();
        assert_eq!(args.limit, Some(10));
    }

    #[test]
    fn project_history_entries_skips_entries_missing_message() {
        // Optional proto field — defensive: a malformed wire entry
        // without `message` shouldn't crash the projection.
        let entries = vec![HistoryEntry {
            seq: 1,
            ts: "2026-01-01T00:00:00Z".into(),
            message: None,
            tag: None,
        }];
        let projected = project_history_entries(&entries);
        assert!(projected.is_empty());
    }

    #[test]
    fn project_history_entries_preserves_seq_ts_role_and_tag() {
        let entries = vec![HistoryEntry {
            seq: 7,
            ts: "2026-01-01T00:00:00Z".into(),
            message: Some(tightbeam_proto::Message {
                role: "assistant".into(),
                content: vec![],
                tool_calls: vec![],
                tool_call_id: None,
                is_error: None,
            }),
            tag: Some("delegate:abc".into()),
        }];
        let projected = project_history_entries(&entries);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].seq, 7);
        assert_eq!(projected[0].ts, "2026-01-01T00:00:00Z");
        assert_eq!(projected[0].role, "assistant");
        assert_eq!(projected[0].tag, Some("delegate:abc"));
    }

    #[test]
    fn parses_valid_args() {
        let args: LlmCallArgs =
            serde_json::from_str(r#"{"system_prompt":"You are alice.","query":"Hi"}"#).unwrap();
        assert_eq!(args.system_prompt, "You are alice.");
        assert_eq!(args.query, "Hi");
    }

    #[test]
    fn rejects_missing_field() {
        let result: Result<LlmCallArgs, _> = serde_json::from_str(r#"{"system_prompt":"alice"}"#);
        assert!(result.is_err());
    }

}
