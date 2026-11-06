//! Harness-local runtime tools: `ReadInstructions`, `ListInstructions`,
//! `Think`, and `RecentTurns`.
//!
//! These are framework-defined tools the LLM can call. The harness advertises
//! them alongside the toolset-served tools and dispatches them in-process.
//! Instruction content is read directly from this agent's mounted instructions
//! volume as an arbitrary read-only file tree; they never fabricate results.
//!
//! `ReadInstructions(path, offset?, limit?)` returns the body of any file in the
//! tree by relative path, optionally sliced to a 1-based line window.
//!
//! `ListInstructions()` returns a flat, sorted, capped list of the tree's file
//! paths. The client command menu passes `{"detail":true}` to get
//! `[{name, description}]`.

use proto_common::{content_text, CallToolResponse, ToolInfo};
use serde::{Deserialize, Serialize};

use crate::agent::text_block;
use crate::instructions::{first_paragraph, slice_lines, Instructions, InstructionsError};
use crate::registry::ConversationRegistry;

// Test-only name the `mod tests` block reaches through `use super::*`.
#[cfg(test)]
use crate::agent::collect_text;

const READ_INSTRUCTIONS_TOOL_NAME: &str = "ReadInstructions";
const LIST_INSTRUCTIONS_TOOL_NAME: &str = "ListInstructions";
pub(crate) const THINK_TOOL_NAME: &str = "Think";
pub(crate) const RECENT_TURNS_TOOL_NAME: &str = "RecentTurns";

/// Cap on the number of paths `ListInstructions` returns.
const LIST_CAP: usize = 1000;

/// Terminal control-flow carrier for the dispatch chain. `Error` folds into an
/// `is_error` tool result the orchestrator loop continues on; `Cancelled` is a
/// distinct terminal signal that drives the whole turn to Cancelled and must
/// never ride the `is_error` funnel.
#[derive(Debug)]
pub(crate) enum DispatchAbort {
    Error(String),
    Cancelled,
}

/// Static definitions advertised by the router at construction time.
pub(crate) fn tool_definitions() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            toolset: String::new(),
            name: READ_INSTRUCTIONS_TOOL_NAME.into(),
            description: "Read a file from the agent instruction tree by relative path and \
                          return its contents. Optional 1-based `offset`/`limit` line numbers \
                          return only that window of lines."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path of the file within the agent instruction tree (nested paths allowed)."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "1-based first line to return; omit to start at the top."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max number of lines to return from `offset`; omit for the rest of the file."
                    }
                },
                "required": ["path"]
            })
            .to_string(),
        },
        ToolInfo {
            toolset: String::new(),
            name: LIST_INSTRUCTIONS_TOOL_NAME.into(),
            description: "List the file paths available in this agent's instruction tree, as \
                          a flat sorted array of relative paths."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            })
            .to_string(),
        },
        ToolInfo {
            toolset: String::new(),
            name: THINK_TOOL_NAME.into(),
            description: "Record a brief observation or piece of reasoning mid-run without \
                          taking any external action. Use this in place of any narrative remark \
                          you would otherwise emit as plain text — a classification sentence, \
                          a counter assignment, a reason a file is unclassifiable, a brief plan \
                          for the next step. The runtime treats every assistant turn as a tool \
                          call; routing your narration through this tool keeps the loop \
                          progressing."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "note": {
                        "type": "string",
                        "description": "The observation or reasoning to record."
                    }
                },
                "required": ["note"]
            })
            .to_string(),
        },
        ToolInfo {
            toolset: String::new(),
            name: RECENT_TURNS_TOOL_NAME.into(),
            description: "Read the most recent turns of the current conversation \
                          (oldest-to-newest). Read-only — use it to recall earlier \
                          context in a long thread. Optional `limit` caps how many \
                          recent turns are returned."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Max number of recent turns to return; omit for all."
                    }
                },
                "required": []
            })
            .to_string(),
        },
    ]
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize, Default)]
struct ListArgs {
    /// When set, return `[{name, description}]` instead of bare paths. The
    /// client's command menu sets it; the LLM's advertised schema omits it, so
    /// the default stays a paths array.
    #[serde(default)]
    detail: bool,
}

#[derive(Serialize)]
struct ListEntryInfo {
    name: String,
    description: String,
}

#[derive(Deserialize)]
struct ThinkArgs {
    note: String,
}

#[derive(Deserialize)]
struct RecentTurnsArgs {
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Serialize)]
struct RecentTurnJson {
    seq: u64,
    ts: String,
    role: String,
    text: String,
}

/// Entrypoint used by `ToolRouter::call_tool` for `Runtime`-source tools.
pub(crate) async fn dispatch(
    name: &str,
    input_json: &str,
    instructions: &Instructions,
    agent: &str,
    registry: &ConversationRegistry,
    conversation_id: &str,
) -> Result<CallToolResponse, DispatchAbort> {
    match name {
        READ_INSTRUCTIONS_TOOL_NAME => dispatch_read(input_json, instructions, agent),
        LIST_INSTRUCTIONS_TOOL_NAME => dispatch_list(input_json, instructions, agent),
        THINK_TOOL_NAME => dispatch_think(input_json).map_err(DispatchAbort::Error),
        RECENT_TURNS_TOOL_NAME => dispatch_recent_turns(input_json, registry, conversation_id)
            .await
            .map_err(DispatchAbort::Error),
        other => Err(DispatchAbort::Error(format!(
            "unknown runtime tool: {other}"
        ))),
    }
}

/// `ReadInstructions(path, offset?, limit?)`: return a file's body from the
/// agent instruction tree with any frontmatter stripped, optionally sliced to
/// a 1-based window of body lines. A missing/invalid/escaping path folds into
/// an `is_error` tool result the LLM sees; an I/O failure is a true infra
/// abort.
fn dispatch_read(
    input_json: &str,
    instructions: &Instructions,
    agent: &str,
) -> Result<CallToolResponse, DispatchAbort> {
    let args: ReadArgs = serde_json::from_str(input_json)
        .map_err(|e| DispatchAbort::Error(format!("invalid read arguments: {e}")))?;
    match instructions.read(agent, &args.path, None, None) {
        Ok(raw) => Ok(CallToolResponse {
            content: vec![text_block(slice_lines(
                &crate::conversation::strip_frontmatter(&raw),
                args.offset,
                args.limit,
            ))],
            is_error: false,
        }),
        Err(InstructionsError::NotFound) => Ok(CallToolResponse {
            content: vec![text_block(format!("file not found: {}", args.path))],
            is_error: true,
        }),
        Err(InstructionsError::InvalidName(n)) => Ok(CallToolResponse {
            content: vec![text_block(format!("invalid path: {n}"))],
            is_error: true,
        }),
        Err(InstructionsError::PathEscape) => Ok(CallToolResponse {
            content: vec![text_block("path escapes agent root".into())],
            is_error: true,
        }),
        Err(InstructionsError::Io(e)) => Err(DispatchAbort::Error(format!("io error: {e}"))),
    }
}

/// `ListInstructions()`: the agent tree's flat sorted file paths, capped.
/// `{detail:true}` returns `[{name, description}]` (path plus the file's first
/// paragraph); the default returns a bare paths array.
fn dispatch_list(
    input_json: &str,
    instructions: &Instructions,
    agent: &str,
) -> Result<CallToolResponse, DispatchAbort> {
    // Empty input_json is the historical "no args" form; treat it as `{}`.
    let args: ListArgs = if input_json.trim().is_empty() {
        ListArgs::default()
    } else {
        serde_json::from_str(input_json)
            .map_err(|e| DispatchAbort::Error(format!("invalid list arguments: {e}")))?
    };
    let paths = instructions.list_tree(agent, LIST_CAP);
    let json = if args.detail {
        let mut infos = Vec::with_capacity(paths.len());
        for path in paths {
            // Best-effort description: a file that vanished mid-enumeration is
            // skipped, not fatal. Strip frontmatter so the description is the
            // file's prose, not its dispatch metadata.
            if let Ok(raw) = instructions.read(agent, &path, None, None) {
                let body = crate::conversation::strip_frontmatter(&raw);
                infos.push(ListEntryInfo {
                    name: path,
                    description: first_paragraph(&body),
                });
            }
        }
        serde_json::to_string(&infos)
            .map_err(|e| DispatchAbort::Error(format!("serialize: {e}")))?
    } else {
        serde_json::to_string(&paths)
            .map_err(|e| DispatchAbort::Error(format!("serialize: {e}")))?
    };
    Ok(CallToolResponse {
        content: vec![text_block(json)],
        is_error: false,
    })
}

/// Read-only tail of the current conversation. Reads the persisted log via
/// the registry snapshot; never mutates. Returns a JSON array of recent
/// turns (oldest-to-newest).
async fn dispatch_recent_turns(
    input_json: &str,
    registry: &ConversationRegistry,
    conversation_id: &str,
) -> Result<CallToolResponse, String> {
    let args: RecentTurnsArgs = match serde_json::from_str(input_json) {
        Ok(a) => a,
        Err(e) => {
            return Ok(CallToolResponse {
                content: vec![text_block(format!(
                    "RecentTurns error: invalid arguments: {e}"
                ))],
                is_error: true,
            })
        }
    };
    let log = registry
        .get_or_create(conversation_id)
        .await
        .map_err(|e| format!("load conversation: {e}"))?;
    let snap = log.read().await.snapshot(args.limit);
    let turns: Vec<RecentTurnJson> = snap
        .entries
        .into_iter()
        .map(|e| RecentTurnJson {
            seq: e.seq,
            ts: e.ts,
            role: e.message.role.clone(),
            text: content_text(&e.message.content),
        })
        .collect();
    let output =
        serde_json::to_string(&turns).map_err(|e| format!("serialize RecentTurns output: {e}"))?;
    Ok(CallToolResponse {
        content: vec![text_block(output)],
        is_error: false,
    })
}

/// In-process echo. Parses `{note}`, returns "noted: <note>". No I/O.
/// The point of this tool isn't the output — it's giving the model a
/// legal tool-shaped place to record reasoning instead of emitting
/// plain text and ending the turn.
#[allow(clippy::unnecessary_wraps)] // Result keeps the dispatch arm uniform with the other tools
fn dispatch_think(input_json: &str) -> Result<CallToolResponse, String> {
    match serde_json::from_str::<ThinkArgs>(input_json) {
        Ok(args) => Ok(CallToolResponse {
            content: vec![text_block(format!("noted: {}", args.note))],
            is_error: false,
        }),
        Err(e) => Ok(CallToolResponse {
            content: vec![text_block(format!("Think error: invalid arguments: {e}"))],
            is_error: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::Instructions;
    use std::path::Path;
    use tempfile::TempDir;

    const AG: &str = "ag";

    fn write_md(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
    }

    /// A temp-dir-backed instructions with the agent root pre-created (so a
    /// missing file surfaces NotFound, not a missing-dir empty list).
    fn empty_instructions() -> (TempDir, Instructions) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(AG)).unwrap();
        let instructions = Instructions::new(tmp.path());
        (tmp, instructions)
    }

    fn test_registry() -> ConversationRegistry {
        use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
        let root = tempfile::TempDir::new().unwrap().keep();
        let factory: std::sync::Arc<dyn ConversationStoreFactory> =
            std::sync::Arc::new(LocalFsFactory::new(root));
        ConversationRegistry::new(factory)
    }

    /// Dispatch against an in-process instructions with a throwaway registry.
    async fn run_dispatch(
        name: &str,
        input: &str,
        instructions: &Instructions,
        parent: &str,
    ) -> Result<CallToolResponse, DispatchAbort> {
        let registry = test_registry();
        dispatch(name, input, instructions, AG, &registry, parent).await
    }

    #[tokio::test]
    async fn dispatch_think_echoes_note() {
        let (_tmp, instructions) = empty_instructions();
        let resp = run_dispatch(
            "Think",
            r#"{"note":"file 1 looks like an assignation"}"#,
            &instructions,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert!(collect_text(&resp.content).contains("file 1 looks like an assignation"));
        assert!(collect_text(&resp.content).starts_with("noted:"));
    }

    #[tokio::test]
    async fn dispatch_list_instructions_detail_returns_name_and_description_json() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(
            tmp.path(),
            "ag/skills/classify.md",
            "# Classify\n\nDecide the doctype and date.\n",
        );
        let instructions = Instructions::new(tmp.path());
        let resp = run_dispatch(
            "ListInstructions",
            r#"{"detail":true}"#,
            &instructions,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        let infos: Vec<serde_json::Value> =
            serde_json::from_str(&collect_text(&resp.content)).unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0]["name"], "skills/classify.md");
        assert_eq!(infos[0]["description"], "Decide the doctype and date.");
    }

    #[tokio::test]
    async fn recent_turns_reads_log_tail_without_mutating() {
        use proto_common::{text_content, Message};
        let (_tmp, instructions) = empty_instructions();
        let registry = test_registry();
        let id = registry.mint("test-owner").await.unwrap();
        let log = registry.get_or_create(&id).await.unwrap();
        log.write()
            .await
            .append(Message {
                role: "user".into(),
                content: text_content("first"),
                tool_calls: vec![],
                tool_call_id: None,
                is_error: None,
            })
            .await
            .unwrap();

        let resp = dispatch(
            "RecentTurns",
            r#"{"limit":5}"#,
            &instructions,
            AG,
            &registry,
            &id,
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        let parsed: Vec<RecentTurnJson> =
            serde_json::from_str(&collect_text(&resp.content)).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].role, "user");
        assert_eq!(parsed[0].text, "first");
        assert_eq!(log.read().await.len(), 1);
    }

    impl<'de> Deserialize<'de> for RecentTurnJson {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            #[derive(Deserialize)]
            struct Helper {
                seq: u64,
                ts: String,
                role: String,
                text: String,
            }
            let h = Helper::deserialize(deserializer)?;
            Ok(RecentTurnJson {
                seq: h.seq,
                ts: h.ts,
                role: h.role,
                text: h.text,
            })
        }
    }

    #[tokio::test]
    async fn read_instructions_on_a_nested_file_with_frontmatter_returns_the_body_only() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(
            tmp.path(),
            "ag/agents/reviewer/AGENTS.md",
            "---\nmodel: other\ntools: [X]\n---\nfirst body line\nsecond body line\n",
        );
        let instructions = Instructions::new(tmp.path());

        let resp = run_dispatch(
            "ReadInstructions",
            r#"{"path":"agents/reviewer/AGENTS.md"}"#,
            &instructions,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert_eq!(
            collect_text(&resp.content),
            "first body line\nsecond body line\n"
        );

        // The line window counts body lines, not the stripped frontmatter's.
        let resp = run_dispatch(
            "ReadInstructions",
            r#"{"path":"agents/reviewer/AGENTS.md","offset":2,"limit":1}"#,
            &instructions,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert!(
            collect_text(&resp.content).contains("second body line"),
            "{}",
            collect_text(&resp.content)
        );
        assert!(!collect_text(&resp.content).contains("first body line"));
    }

    #[tokio::test]
    async fn dispatch_unknown_runtime_tool_returns_err() {
        let (_tmp, instructions) = empty_instructions();
        let err = run_dispatch("Ghost", "{}", &instructions, "parent")
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchAbort::Error(ref e) if e.contains("unknown runtime tool")));
    }
}
