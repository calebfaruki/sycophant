//! Harness-local runtime tools: `ReadInstructions`, `ListInstructions`,
//! `Think` and `ReadArchive`.
//!
//! These are framework-defined tools the LLM can call. The harness advertises
//! them alongside the toolset-served tools and dispatches them in-process.
//! Instruction content is read directly from this agent's mounted instructions
//! volume as an arbitrary read-only file tree; they never fabricate results.

use proto_common::{content_text, text_block, CallToolResponse, ToolInfo};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::agent_state::AgentState;
use crate::conversation::{tool_turn_as_text, ConversationLog};
use crate::instructions::{page, slice_lines, strip_frontmatter, Instructions, InstructionsError};

const READ_INSTRUCTIONS_TOOL_NAME: &str = "ReadInstructions";
const LIST_INSTRUCTIONS_TOOL_NAME: &str = "ListInstructions";
const THINK_TOOL_NAME: &str = "Think";
const READ_ARCHIVE_TOOL_NAME: &str = "ReadArchive";

/// Cap on the paths `ListInstructions` returns and the primary prompt's tree map
/// lists.
pub(crate) const LIST_CAP: usize = 1000;

/// Terminal control-flow carrier for the dispatch chain. `Error` folds into an
/// `is_error` tool result the agent loop continues on; `Cancelled` is a
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
                          for the next step. Plain text ends your turn; call this to record \
                          reasoning and keep working."
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
            name: READ_ARCHIVE_TOOL_NAME.into(),
            description: "Read an archived part of this conversation. Each compact archives \
                          the conversation before it; `archive` 1 is the newest, 2 the one \
                          before it, and so on. A clear keeps no archive. Returns its entries \
                          oldest-to-newest. Optional 1-based `offset`/`limit` return only that \
                          window of entries."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "archive": {
                        "type": "integer",
                        "description": "Number of the archive to read; 1 is the newest."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "1-based first entry to return; omit to start at the first."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max number of entries to return from `offset`; omit for the rest."
                    }
                },
                "required": ["archive"]
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

#[derive(Deserialize)]
struct ThinkArgs {
    note: String,
}

#[derive(Deserialize)]
struct ReadArchiveArgs {
    archive: u64,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Serialize)]
struct EntryJson {
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
    agent_state: &AgentState,
) -> Result<CallToolResponse, String> {
    match name {
        READ_INSTRUCTIONS_TOOL_NAME => dispatch_read(input_json, instructions, agent),
        LIST_INSTRUCTIONS_TOOL_NAME => dispatch_list(instructions, agent),
        THINK_TOOL_NAME => Ok(dispatch_think(input_json)),
        READ_ARCHIVE_TOOL_NAME => dispatch_read_archive(input_json, agent_state.log()).await,
        other => Err(format!("unknown runtime tool: {other}")),
    }
}

/// `ReadInstructions(path, offset?, limit?)`: return a file's body from the
/// agent instruction tree with any frontmatter stripped, optionally sliced to
/// a 1-based window of body lines. Every failure is an `is_error` tool result
/// the LLM sees.
fn dispatch_read(
    input_json: &str,
    instructions: &Instructions,
    agent: &str,
) -> Result<CallToolResponse, String> {
    let args: ReadArgs =
        serde_json::from_str(input_json).map_err(|e| format!("invalid read arguments: {e}"))?;
    match instructions.read(agent, &args.path) {
        Ok(raw) => Ok(CallToolResponse {
            content: vec![text_block(slice_lines(
                &strip_frontmatter(&raw),
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
        Err(InstructionsError::Io(e)) => Err(format!("io error: {e}")),
    }
}

/// `ListInstructions()`: the agent tree's flat sorted file paths, capped.
fn dispatch_list(instructions: &Instructions, agent: &str) -> Result<CallToolResponse, String> {
    let paths = instructions.list_tree(agent, LIST_CAP);
    let json = serde_json::to_string(&paths).map_err(|e| format!("serialize: {e}"))?;
    Ok(CallToolResponse {
        content: vec![text_block(json)],
        is_error: false,
    })
}

/// `ReadArchive(archive, offset?, limit?)`: the entries of archive
/// `conversation.log.<archive>.gz` (1 the newest), paged by [`page`].
/// The file name is built from the integer alone, so the tool reaches neither
/// the current window nor any other name at the root. A missing archive (0 included) and
/// every read failure are error results.
async fn dispatch_read_archive(
    input_json: &str,
    log: &RwLock<ConversationLog>,
) -> Result<CallToolResponse, String> {
    let error = |text: String| -> Result<CallToolResponse, String> {
        Ok(CallToolResponse {
            content: vec![text_block(text)],
            is_error: true,
        })
    };
    let args: ReadArchiveArgs = match serde_json::from_str(input_json) {
        Ok(a) => a,
        Err(e) => return error(format!("ReadArchive error: invalid arguments: {e}")),
    };
    let n = args.archive;
    let entries = match log.read().await.read_archive(n).await {
        Ok(Some(entries)) => entries,
        Ok(None) => return error(format!("ReadArchive error: no archive {n}")),
        Err(e) => return error(format!("ReadArchive error: archive {n} {e}")),
    };
    let entries: Vec<EntryJson> = page(entries, args.offset, args.limit)
        .into_iter()
        .map(|e| EntryJson {
            seq: e.seq,
            ts: e.ts,
            // The rewrite turns a tool result into a user turn; keep the stored role.
            role: e.message.role.clone(),
            text: content_text(&tool_turn_as_text(e.message).content),
        })
        .collect();
    let output = serde_json::to_string(&entries)
        .map_err(|e| format!("serialize ReadArchive output: {e}"))?;
    Ok(CallToolResponse {
        content: vec![text_block(output)],
        is_error: false,
    })
}

fn dispatch_think(input_json: &str) -> CallToolResponse {
    match serde_json::from_str::<ThinkArgs>(input_json) {
        Ok(args) => CallToolResponse {
            content: vec![text_block(format!("noted: {}", args.note))],
            is_error: false,
        },
        Err(e) => CallToolResponse {
            content: vec![text_block(format!("Think error: invalid arguments: {e}"))],
            is_error: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ConversationStore, LogEntry};
    use crate::test_doubles::{empty_agent_state, record, test_instructions, write_md, TEST_AG};

    /// Dispatch against an in-process instructions with a throwaway agent state.
    async fn run_dispatch(
        name: &str,
        input: &str,
        instructions: &Instructions,
    ) -> Result<CallToolResponse, String> {
        let agent_state = empty_agent_state();
        dispatch(name, input, instructions, TEST_AG, &agent_state).await
    }

    #[tokio::test]
    async fn dispatch_think_echoes_note() {
        let instructions = test_instructions();
        let resp = run_dispatch(
            "Think",
            r#"{"note":"file 1 looks like an assignation"}"#,
            &instructions,
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert!(content_text(&resp.content).contains("file 1 looks like an assignation"));
        assert!(content_text(&resp.content).starts_with("noted:"));
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
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert_eq!(
            content_text(&resp.content),
            "first body line\nsecond body line\n"
        );

        // The line window counts body lines, not the stripped frontmatter's.
        let resp = run_dispatch(
            "ReadInstructions",
            r#"{"path":"agents/reviewer/AGENTS.md","offset":2,"limit":1}"#,
            &instructions,
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert!(
            content_text(&resp.content).contains("second body line"),
            "{}",
            content_text(&resp.content)
        );
        assert!(!content_text(&resp.content).contains("first body line"));
    }

    #[tokio::test]
    async fn dispatch_unknown_runtime_tool_returns_err() {
        let instructions = test_instructions();
        let err = run_dispatch("Ghost", "{}", &instructions)
            .await
            .unwrap_err();
        assert!(err.contains("unknown runtime tool"), "{err}");
    }

    // ReadArchive is offered, and the model must name an archive. With no archive on disk, archive 0 (logrotate never makes
    // one) and archive 3 are each the exact "no archive" error result, a
    // non-integer is an invalid-arguments error result, and none of them reads
    // the current conversation.log.
    #[tokio::test]
    async fn read_archive_is_offered_and_refuses_zero_a_missing_archive_and_a_non_integer() {
        let defs = tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        let read_archive = defs
            .iter()
            .find(|d| d.name == "ReadArchive")
            .unwrap_or_else(|| panic!("tool_definitions offers ReadArchive, got {names:?}"));
        let params: serde_json::Value =
            serde_json::from_str(&read_archive.parameters_json).unwrap();
        assert_eq!(
            params["required"],
            serde_json::json!(["archive"]),
            "ReadArchive requires `archive`"
        );

        let instructions = test_instructions();
        let root_dir = tempfile::TempDir::new().unwrap();
        let root = root_dir.path();
        std::fs::write(
            root.join("conversation.log"),
            record("user", "current-window"),
        )
        .unwrap();
        let agent_state = AgentState::load(root.to_path_buf(), 10).await.unwrap();

        let error_text = |resp: Result<CallToolResponse, String>, input: &str| -> String {
            match resp {
                Ok(r) => {
                    let text = content_text(&r.content);
                    assert!(r.is_error, "{input} is an error result, got {text:?}");
                    assert!(
                        !text.contains("current-window"),
                        "{input} never reads conversation.log, got {text:?}"
                    );
                    text
                }
                Err(e) => panic!("{input} is an error result, not a dispatch error: {e}"),
            }
        };
        for (input, want) in [
            (r#"{"archive":0}"#, "ReadArchive error: no archive 0"),
            (r#"{"archive":3}"#, "ReadArchive error: no archive 3"),
        ] {
            let resp = dispatch("ReadArchive", input, &instructions, TEST_AG, &agent_state).await;
            assert_eq!(error_text(resp, input), want);
        }
        let input = r#"{"archive":"one"}"#;
        let resp = dispatch("ReadArchive", input, &instructions, TEST_AG, &agent_state).await;
        let text = error_text(resp, input);
        assert!(
            text.starts_with("ReadArchive error: invalid arguments: "),
            "{input} is an invalid-arguments error, got {text:?}"
        );
    }

    /// A store whose one archive, 1, holds five entries: a user turn, an
    /// assistant reply, an assistant turn that only calls a tool, that tool's
    /// error result and a last user turn.
    struct OneArchive;

    #[async_trait::async_trait]
    impl ConversationStore for OneArchive {
        async fn append_event(&self, _: &LogEntry) -> Result<(), String> {
            unreachable!()
        }
        async fn read_all(&self) -> Result<Vec<LogEntry>, String> {
            unreachable!()
        }
        async fn clear(&self) -> Result<(), String> {
            unreachable!()
        }
        async fn archive(&self) -> Result<(), String> {
            unreachable!()
        }
        async fn read_archive(&self, n: u64) -> Result<Option<Vec<LogEntry>>, String> {
            Ok((n == 1).then(|| {
                let entry = |role: &str, text: &str| -> LogEntry {
                    serde_json::from_str(&record(role, text)).unwrap()
                };
                let mut call = entry("assistant", "");
                call.content.clear();
                call.tool_calls = vec![proto_common::ToolCall {
                    id: "call-1".into(),
                    name: "ReadInstructions".into(),
                    input_json: r#"{"path":"a.md"}"#.into(),
                }];
                let mut result = entry("tool", "file not found: a.md");
                result.tool_call_id = Some("call-1".into());
                result.is_error = Some(true);
                vec![
                    entry("user", "a"),
                    entry("assistant", "b"),
                    call,
                    result,
                    entry("user", "c"),
                ]
            }))
        }
    }

    #[tokio::test]
    async fn read_archive_returns_the_paged_entries_as_json() {
        let log = RwLock::new(ConversationLog::new(std::sync::Arc::new(OneArchive)));
        let resp = dispatch_read_archive(r#"{"archive":1,"offset":2,"limit":3}"#, &log)
            .await
            .unwrap();
        assert!(!resp.is_error, "{}", content_text(&resp.content));
        let got: serde_json::Value = serde_json::from_str(&content_text(&resp.content)).unwrap();
        assert_eq!(
            got,
            serde_json::json!([
                {"seq": 2, "ts": "t", "role": "assistant", "text": "b"},
                {
                    "seq": 3,
                    "ts": "t",
                    "role": "assistant",
                    "text": "[tool call ReadInstructions: {\"path\":\"a.md\"}]"
                },
                {"seq": 4, "ts": "t", "role": "tool", "text": "[tool error]\nfile not found: a.md"}
            ])
        );
    }
}
