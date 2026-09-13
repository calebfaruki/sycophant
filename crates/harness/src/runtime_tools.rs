//! Harness-local runtime tools: `read`, `dispatch`, `list`, `Think`, and
//! `RecentTurns`.
//!
//! These are framework-defined tools the LLM can call. The harness advertises
//! them alongside the toolset-served tools and dispatches them in-process.
//! Instruction content is read directly from this workspace's mounted kernel
//! volume as an arbitrary read-only file tree; they never fabricate results.
//!
//! `read(path, offset?, limit?)` returns the body of any file in the tree by
//! relative path, optionally sliced to a 1-based line window.
//!
//! `dispatch(path, query)` loads any instruction file, strips its frontmatter
//! into runtime configuration, and runs a fresh isolated sub-turn whose model
//! and toolset tools come only from that frontmatter, with the stripped body as
//! the system prompt and `query` as the user input. Every sub-turn is advertised
//! the always-on runtime substrate (`read`/`dispatch`/`list`/`Think`) on top of
//! the frontmatter-named toolset tools, and runs a bounded tool-use loop until
//! it returns a final response. There is no single-shot mode.
//!
//! `list()` returns a flat, sorted, capped list of the tree's file paths. The
//! client command menu passes `{"detail":true}` to get `[{name, description}]`.

use proto_common::{content_text, CallToolResponse, Message, ToolDefinition, ToolInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use toolset_proto::{TurnRequest, TurnRole};

use crate::agent::{self, text_block, LoopError, LoopHalt, LoopMode};
use crate::clients::{RelayRpc, ToolsetRpc};
use crate::conversation::HistoryScope;
use crate::kernel::{first_paragraph, Kernel, KernelError};
use crate::registry::ConversationRegistry;
use crate::tool_router::ToolDispatcher;
use crate::turn;

// Test-only names the `mod tests` block reaches through `use super::*`: the
// product path no longer references them since the sub-turn loop is
// `agent::llm_loop`, which owns stop-reason handling and text collection.
#[cfg(test)]
use crate::agent::collect_text;
#[cfg(test)]
use proto_common::StopReason;

const READ_TOOL_NAME: &str = "read";
const DISPATCH_TOOL_NAME: &str = "dispatch";
const LIST_TOOL_NAME: &str = "list";
pub(crate) const THINK_TOOL_NAME: &str = "Think";
pub(crate) const RECENT_TURNS_TOOL_NAME: &str = "RecentTurns";

/// Cap on the number of paths `list` returns.
const LIST_CAP: usize = 1000;

/// Upper bound on a dispatched sub-turn's tool-use rounds, mirroring the
/// orchestrator loop's iteration guard so a granted-tools sub-turn cannot spin
/// forever.
const MAX_SUBTURN_ITERATIONS: u32 = 16;

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
            name: READ_TOOL_NAME.into(),
            description: "Read a file from the workspace instruction tree by relative path and \
                          return its contents. Optional 1-based `offset`/`limit` line numbers \
                          return only that window of lines."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path of the file within the workspace instruction tree (nested paths allowed)."
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
            name: DISPATCH_TOOL_NAME.into(),
            description: "Dispatch a sub-turn defined by an instruction file: load the file at \
                          the given path, use its body as the system prompt, and run the query \
                          against the model and tools its frontmatter names. Returns the \
                          sub-turn's final response text."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path of the instruction file to dispatch (nested paths allowed)."
                    },
                    "query": {
                        "type": "string",
                        "description": "The question or instruction to send to the dispatched sub-turn."
                    }
                },
                "required": ["path", "query"]
            })
            .to_string(),
        },
        ToolInfo {
            toolset: String::new(),
            name: LIST_TOOL_NAME.into(),
            description: "List the file paths available in this workspace's instruction tree, as \
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

#[derive(Deserialize)]
struct DispatchArgs {
    path: String,
    query: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
}

/// Entrypoint used by `ToolRouter::call_tool` for `Runtime`-source tools.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch(
    name: &str,
    input_json: &str,
    kernel: &Kernel,
    workspace: &str,
    toolset: &mut dyn ToolsetRpc,
    tool_router: &dyn ToolDispatcher,
    registry: &ConversationRegistry,
    parent_conversation_id: &str,
    reply_channel: Option<&str>,
    relay: Option<&mut dyn RelayRpc>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<CallToolResponse, DispatchAbort> {
    match name {
        READ_TOOL_NAME => dispatch_read(input_json, kernel, workspace),
        LIST_TOOL_NAME => dispatch_list(input_json, kernel, workspace),
        DISPATCH_TOOL_NAME => {
            dispatch_verb(
                input_json,
                kernel,
                workspace,
                toolset,
                tool_router,
                registry,
                parent_conversation_id,
                reply_channel,
                relay,
                cancel,
            )
            .await
            .or_else(|abort| match abort {
                // Tool-call errors flow back to the LLM as is_error tool
                // results; only true infra failures escape via Err.
                DispatchAbort::Error(e) => Ok(CallToolResponse {
                    content: vec![text_block(format!("dispatch error: {e}"))],
                    is_error: true,
                }),
                // A cancelled sub-turn is terminal — it must NOT fold into an
                // is_error result the loop continues on. Propagate it.
                DispatchAbort::Cancelled => Err(DispatchAbort::Cancelled),
            })
        }
        THINK_TOOL_NAME => dispatch_think(input_json).map_err(DispatchAbort::Error),
        RECENT_TURNS_TOOL_NAME => {
            dispatch_recent_turns(input_json, registry, parent_conversation_id)
                .await
                .map_err(DispatchAbort::Error)
        }
        other => Err(DispatchAbort::Error(format!(
            "unknown runtime tool: {other}"
        ))),
    }
}

/// `read(path, offset?, limit?)`: return a file's body from the workspace
/// instruction tree, optionally sliced to a 1-based line window. A
/// missing/invalid/escaping path folds into an `is_error` tool result the LLM
/// sees; an I/O failure is a true infra abort.
fn dispatch_read(
    input_json: &str,
    kernel: &Kernel,
    workspace: &str,
) -> Result<CallToolResponse, DispatchAbort> {
    let args: ReadArgs = serde_json::from_str(input_json)
        .map_err(|e| DispatchAbort::Error(format!("invalid read arguments: {e}")))?;
    match kernel.read(workspace, &args.path, args.offset, args.limit) {
        Ok(body) => Ok(CallToolResponse {
            content: vec![text_block(body)],
            is_error: false,
        }),
        Err(KernelError::NotFound) => Ok(CallToolResponse {
            content: vec![text_block(format!("file not found: {}", args.path))],
            is_error: true,
        }),
        Err(KernelError::InvalidName(n)) => Ok(CallToolResponse {
            content: vec![text_block(format!("invalid path: {n}"))],
            is_error: true,
        }),
        Err(KernelError::PathEscape) => Ok(CallToolResponse {
            content: vec![text_block("path escapes workspace root".into())],
            is_error: true,
        }),
        Err(KernelError::Io(e)) => Err(DispatchAbort::Error(format!("io error: {e}"))),
    }
}

/// `list()`: the workspace tree's flat sorted file paths, capped. `{detail:true}`
/// returns `[{name, description}]` (path plus the file's first paragraph); the
/// default returns a bare paths array.
fn dispatch_list(
    input_json: &str,
    kernel: &Kernel,
    workspace: &str,
) -> Result<CallToolResponse, DispatchAbort> {
    // Empty input_json is the historical "no args" form; treat it as `{}`.
    let args: ListArgs = if input_json.trim().is_empty() {
        ListArgs::default()
    } else {
        serde_json::from_str(input_json)
            .map_err(|e| DispatchAbort::Error(format!("invalid list arguments: {e}")))?
    };
    let paths = kernel.list_tree(workspace, LIST_CAP);
    let json = if args.detail {
        let mut infos = Vec::with_capacity(paths.len());
        for path in paths {
            // Best-effort description: a file that vanished mid-enumeration is
            // skipped, not fatal. Strip frontmatter so the description is the
            // file's prose, not its dispatch metadata.
            if let Ok(raw) = kernel.read(workspace, &path, None, None) {
                let (body, _) = crate::conversation::strip_frontmatter(&raw);
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
            tag: e.tag,
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

/// `dispatch(path, query)`: read the instruction file at `path`, strip its
/// frontmatter into runtime configuration, and run a fresh isolated sub-turn on
/// the file's model with its body as the system prompt. The sub-turn is
/// advertised the always-on runtime substrate plus the toolset tools the
/// frontmatter names — the same scoping the primary turn applies via
/// `ToolDispatcher::tool_definitions_scoped` — and runs a bounded tool-use loop
/// (`agent::llm_loop`) until it returns a final response. There is no
/// single-shot mode. Model and tools come only from the file, never from the
/// verb arguments.
#[allow(clippy::too_many_arguments)]
async fn dispatch_verb(
    input_json: &str,
    kernel: &Kernel,
    workspace: &str,
    toolset: &mut dyn ToolsetRpc,
    tool_router: &dyn ToolDispatcher,
    registry: &ConversationRegistry,
    parent_conversation_id: &str,
    reply_channel: Option<&str>,
    relay: Option<&mut dyn RelayRpc>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<CallToolResponse, DispatchAbort> {
    let args: DispatchArgs = serde_json::from_str(input_json)
        .map_err(|e| DispatchAbort::Error(format!("invalid dispatch arguments: {e}")))?;

    // The whole file, read by arbitrary relative path through the confined
    // reader. Errors that name the path fold to an LLM-visible tool error; only
    // an I/O failure is a true infra abort.
    let file_text = match kernel.read(workspace, &args.path, None, None) {
        Ok(text) => text,
        Err(KernelError::NotFound) => {
            return Err(DispatchAbort::Error(format!(
                "dispatch target not found: {}",
                args.path
            )))
        }
        Err(KernelError::InvalidName(n)) => {
            return Err(DispatchAbort::Error(format!("invalid dispatch path: {n}")))
        }
        Err(KernelError::PathEscape) => {
            return Err(DispatchAbort::Error(
                "dispatch path escapes workspace root".into(),
            ))
        }
        Err(KernelError::Io(e)) => return Err(DispatchAbort::Error(format!("io error: {e}"))),
    };

    // The file's frontmatter is dispatch configuration, not instruction text:
    // the body is what the sub-turn receives as its system prompt.
    let (system_body, frontmatter) = crate::conversation::strip_frontmatter(&file_text);

    // The parent's log is read only for `model: inherit`. It is best-effort: a
    // parent id this harness cannot open (an unregistered or malformed id) just
    // means no inherited model to read, not a dispatch failure. The sub-turn
    // persists into its own child log, opened below.
    let parent_log = registry.get_or_create(parent_conversation_id).await.ok();

    // No fallback: a file that names nothing dispatches with no model and the
    // controller refuses the turn. Nothing in the harness may pick one.
    let model = crate::runtime_entrypoint::resolve_model(
        frontmatter.model.as_deref(),
        parent_log.as_deref(),
    )
    .await;
    let attribution = crate::conversation::AssistantAttribution {
        model: model.clone(),
        // The pre-strip file text, matching the primary turn's hash.
        system_prompt_sha256: Some(crate::conversation::sha256_hex(&file_text)),
        warnings: vec![],
    };

    // Every sub-turn is advertised the always-on runtime substrate plus the
    // toolset tools the frontmatter names, via the same router seam the primary
    // turn uses. The `tools:` list scopes only toolset tools; it never widens or
    // removes the runtime substrate. There is no empty tool set and no
    // single-shot mode.
    let tools: Vec<ToolDefinition> =
        tool_router.tool_definitions_scoped(frontmatter.tools.as_deref());

    // Sub-conversation id minted locally under the parent's owner so it stays in
    // the same drawer. The child id is minted but never `register_turn`'d: the
    // sub-turn shares the parent turn's `cancel` token, not a second
    // registration. It scopes the delegate history entries `llm_loop` persists,
    // so siblings of one parent stay distinct in the record.
    let child_conversation_id = registry
        .mint(
            &registry
                .owner_of(parent_conversation_id)
                .await
                .unwrap_or_default(),
        )
        .await
        .map_err(DispatchAbort::Error)?;

    // The child's own log. `llm_loop` appends the sub-turn's assistant/tool
    // entries here under the delegate scope. The id was just minted, so this
    // open always succeeds.
    let child_log = registry
        .get_or_create(&child_conversation_id)
        .await
        .map_err(|e| {
            DispatchAbort::Error(format!("dispatch could not open the sub-turn log: {e}"))
        })?;

    // Sub-turn frames relay through a live GatewaySink when the turn has a reply
    // channel; otherwise they drop.
    let mut null_sink = turn::NullSink;
    let mut gateway_sink;
    let sink: &mut dyn turn::StreamSink = match (reply_channel, relay) {
        (Some(channel_id), Some(rpc)) => {
            gateway_sink = turn::GatewaySink {
                rpc,
                channel_id: channel_id.to_string(),
            };
            &mut gateway_sink
        }
        _ => &mut null_sink,
    };
    let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_SUBAGENT_SCRUB__");

    // The sub-turn history is seeded with the query; `llm_loop` resends the
    // assembled history every round (providers are stateless).
    let initial_request = TurnRequest {
        system: Some(system_body),
        tools,
        messages: vec![Message {
            role: "user".into(),
            content: vec![text_block(args.query)],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
        }],
        model,
        reply_channel: None,
        role: Some(TurnRole::Delegate as i32),
        correlation_id: Some(parent_conversation_id.to_string()),
        conversation_id: child_conversation_id.clone(),
    };

    // Reuse the orchestrator loop: it advertises the sub-request's tools, routes
    // every sub-turn tool call through the dispatcher (so a frontmatter-granted
    // toolset tool actually executes and its result feeds back), and persists
    // the sub-turn under the delegate scope. The delegate scope also carries the
    // parent link and the dispatched file path, so `llm_loop` stamps every
    // streamed frame with them and the client nests the sub-turn under its
    // parent. The sub-turn shares the parent turn's cancel token.
    let result = agent::llm_loop(
        MAX_SUBTURN_ITERATIONS,
        toolset,
        tool_router,
        &child_log,
        HistoryScope::Delegate {
            call_id: &child_conversation_id,
            parent_conversation_id,
            agent_name: &args.path,
        },
        attribution,
        initial_request,
        LoopMode {
            reply_channel: None,
            idle_gap: turn::DEFAULT_IDLE_GAP,
            cancel: cancel.clone(),
            grants: HashMap::new(),
        },
        sink,
        &scrub,
    )
    .await;

    match result {
        // A final response or a token-truncated partial both surface as the
        // sub-turn's text result.
        Ok(text) | Err(LoopError::Halt(LoopHalt::MaxTokens(text))) => Ok(CallToolResponse {
            content: vec![text_block(text)],
            is_error: false,
        }),
        // A cancelled sub-turn is terminal: it must NOT fold into an is_error
        // result the caller's loop continues on. Propagate it.
        Err(LoopError::Cancelled) => Err(DispatchAbort::Cancelled),
        Err(LoopError::Halt(LoopHalt::IterationLimit { limit })) => Ok(CallToolResponse {
            content: vec![text_block(format!(
                "sub-agent exceeded {limit} tool iterations"
            ))],
            is_error: true,
        }),
        Err(LoopError::Halt(LoopHalt::UnknownStop(stop))) => Ok(CallToolResponse {
            content: vec![text_block(format!(
                "sub-agent stopped unexpectedly ({stop:?})"
            ))],
            is_error: true,
        }),
        Err(LoopError::ToolDispatch(e)) => Ok(CallToolResponse {
            content: vec![text_block(format!("sub-agent tool dispatch failed: {e}"))],
            is_error: true,
        }),
        // A failed turn RPC or an ended stream is a true infra abort.
        Err(LoopError::ToolsetRpc(e)) | Err(LoopError::StreamEnded(e)) => {
            Err(DispatchAbort::Error(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::TurnSource;
    use crate::kernel::Kernel;
    use crate::tool_router::ToolDispatcher;
    use proto_common::{content_block, ContentBlock, TextBlock, ToolCall};
    use std::collections::{HashMap, VecDeque};
    use std::path::Path;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;
    use toolset_proto::{turn_event, TurnComplete, TurnEvent};

    const WS: &str = "ws";

    fn write_md(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
    }

    /// A temp-dir-backed kernel with the workspace root pre-created (so a
    /// missing file surfaces NotFound, not a missing-dir empty list).
    fn empty_kernel() -> (TempDir, Kernel) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(WS)).unwrap();
        let kernel = Kernel::new(tmp.path());
        (tmp, kernel)
    }

    struct FakeTurnSource {
        events: VecDeque<TurnEvent>,
    }

    #[async_trait::async_trait]
    impl TurnSource for FakeTurnSource {
        async fn next_event(&mut self) -> Option<Result<TurnEvent, String>> {
            self.events.pop_front().map(Ok)
        }
    }

    struct FakeToolset {
        turns: VecDeque<Vec<TurnEvent>>,
        recorded: Vec<TurnRequest>,
    }

    impl FakeToolset {
        fn new(turns: Vec<Vec<TurnEvent>>) -> Self {
            Self {
                turns: turns.into(),
                recorded: Vec::new(),
            }
        }
        fn empty() -> Self {
            Self::new(vec![])
        }
    }

    #[async_trait::async_trait]
    impl ToolsetRpc for FakeToolset {
        async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
            self.recorded.push(request);
            let events = self
                .turns
                .pop_front()
                .ok_or_else(|| "FakeToolset: no more scripted turns".to_string())?;
            Ok(Box::new(FakeTurnSource {
                events: events.into(),
            }))
        }
        async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// A stand-in `ToolDispatcher` for dispatch sub-turn tests. `call_tool`
    /// records every routed call and returns a scripted result, so a test can
    /// assert a sub-turn tool call reached the dispatcher rather than being
    /// rejected as an unknown runtime tool. `tool_definitions_scoped` advertises
    /// the runtime tools plus any toolset tool this dispatcher knows that the
    /// agent's `tools:` list names — the same scoping the primary turn applies.
    struct FakeDispatcher {
        toolset_tools: Vec<String>,
        result: String,
        routed: Mutex<Vec<String>>,
    }

    impl FakeDispatcher {
        fn empty() -> Self {
            Self {
                toolset_tools: Vec::new(),
                result: String::new(),
                routed: Mutex::new(Vec::new()),
            }
        }
        fn with_toolset_tool(name: &str, result: &str) -> Self {
            Self {
                toolset_tools: vec![name.to_string()],
                result: result.to_string(),
                routed: Mutex::new(Vec::new()),
            }
        }
        fn routed(&self) -> Vec<String> {
            self.routed.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl ToolDispatcher for FakeDispatcher {
        async fn call_tool(
            &self,
            name: &str,
            _input_json: &str,
            _grants: &HashMap<String, String>,
            _toolset: &mut dyn ToolsetRpc,
            _conversation_id: &str,
            _reply_channel: Option<&str>,
            _tool_call_id: &str,
            _cancel: &CancellationToken,
        ) -> Result<CallToolResponse, DispatchAbort> {
            self.routed.lock().unwrap().push(name.to_string());
            Ok(CallToolResponse {
                content: vec![text_block(self.result.clone())],
                is_error: false,
            })
        }

        fn tool_definitions_scoped(&self, agent_tools: Option<&[String]>) -> Vec<ToolDefinition> {
            // Runtime tools are always advertised; toolset tools only when the
            // frontmatter names them.
            let mut defs: Vec<ToolDefinition> = tool_definitions()
                .into_iter()
                .map(|t| ToolDefinition {
                    name: t.name,
                    description: t.description,
                    parameters_json: t.parameters_json,
                })
                .collect();
            if let Some(list) = agent_tools {
                for t in &self.toolset_tools {
                    if list.iter().any(|n| n == t) {
                        defs.push(ToolDefinition {
                            name: t.clone(),
                            description: format!("toolset tool {t}"),
                            parameters_json: "{}".into(),
                        });
                    }
                }
            }
            defs
        }
    }

    fn test_registry() -> ConversationRegistry {
        use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
        let root = tempfile::TempDir::new().unwrap().keep();
        let factory: std::sync::Arc<dyn ConversationStoreFactory> =
            std::sync::Arc::new(LocalFsFactory::new(root));
        ConversationRegistry::new(factory)
    }

    /// Dispatch against an in-process kernel with a throwaway registry and a
    /// no-op dispatcher (no toolset tools). Sub-turn tool calls route through
    /// the dispatcher's `call_tool`.
    async fn run_dispatch(
        name: &str,
        input: &str,
        kernel: &Kernel,
        toolset: &mut FakeToolset,
        parent: &str,
    ) -> Result<CallToolResponse, DispatchAbort> {
        let dispatcher = FakeDispatcher::empty();
        run_dispatch_routed(name, input, kernel, toolset, &dispatcher, parent).await
    }

    /// Dispatch with an explicit `ToolDispatcher` so a test can advertise and
    /// route a toolset tool named in the dispatched agent's frontmatter.
    async fn run_dispatch_routed(
        name: &str,
        input: &str,
        kernel: &Kernel,
        toolset: &mut FakeToolset,
        dispatcher: &dyn ToolDispatcher,
        parent: &str,
    ) -> Result<CallToolResponse, DispatchAbort> {
        let registry = test_registry();
        let cancel = tokio_util::sync::CancellationToken::new();
        dispatch(
            name, input, kernel, WS, toolset, dispatcher, &registry, parent, None, None, &cancel,
        )
        .await
    }

    fn complete(stop: StopReason, text: &str) -> Vec<TurnEvent> {
        vec![TurnEvent {
            event: Some(turn_event::Event::Complete(TurnComplete {
                stop_reason: stop as i32,
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
                }],
                tool_calls: vec![],
            })),
        }]
    }

    fn end_turn(text: &str) -> Vec<TurnEvent> {
        complete(StopReason::EndTurn, text)
    }

    // The tool set exposes the generic `read`/`dispatch`/`list` verbs, not the
    // four typed ones.
    #[test]
    fn tool_definitions_exposes_generic_verbs_not_typed_ones() {
        let tools = tool_definitions();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        // Generic path-based verbs are present.
        assert!(names.contains(&"read"), "read verb registered: {names:?}");
        assert!(
            names.contains(&"dispatch"),
            "dispatch verb registered: {names:?}"
        );
        assert!(names.contains(&"list"), "list verb registered: {names:?}");
        // The four typed verbs are gone.
        assert!(!names.contains(&"Agent"), "Agent verb removed: {names:?}");
        assert!(!names.contains(&"Agents"), "Agents verb removed: {names:?}");
        assert!(!names.contains(&"Skill"), "Skill verb removed: {names:?}");
        assert!(!names.contains(&"Skills"), "Skills verb removed: {names:?}");
        // Think/RecentTurns are untouched by this phase.
        assert!(names.contains(&"Think"));
    }

    /// An instruction file with a YAML frontmatter block naming a model and a
    /// tools list, plus a body.
    fn fm_file(model: &str, tools: &[&str], body: &str) -> String {
        let list = tools
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("---\nmodel: {model}\ntools: [{list}]\n---\n{body}")
    }

    #[tokio::test]
    async fn dispatch_reads_nested_path_strips_frontmatter_runs_subturn_returns_result() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(
            tmp.path(),
            "ws/agents/team/scribe.md",
            &fm_file("fixture-model", &[], "scribe body"),
        );
        let kernel = Kernel::new(tmp.path());
        let mut toolset = FakeToolset::new(vec![end_turn("the sub-turn reply")]);
        let resp = run_dispatch(
            "dispatch",
            r#"{"path":"agents/team/scribe.md","query":"hi"}"#,
            &kernel,
            &mut toolset,
            "parent",
        )
        .await
        .expect("the dispatch verb returns a tool result once implemented");
        assert!(!resp.is_error, "got {:?}", collect_text(&resp.content));
        assert_eq!(collect_text(&resp.content), "the sub-turn reply");
        assert_eq!(toolset.recorded.len(), 1);
        let sent = &toolset.recorded[0];
        assert_eq!(
            sent.system.as_deref(),
            Some("scribe body"),
            "the sub-turn's system prompt is the body with its frontmatter stripped"
        );
        assert_eq!(
            sent.model.as_deref(),
            Some("fixture-model"),
            "the sub-turn runs on the model named in the file's frontmatter"
        );
    }

    // A dispatched agent's `tools:` frontmatter scopes the TOOLSET tools
    // advertised to its sub-turn, exactly as the primary turn's
    // `tool_definitions_scoped`. A toolset tool the file names is advertised; one
    // it does not name is scoped out.
    #[tokio::test]
    async fn dispatch_scopes_frontmatter_toolset_tool_into_the_subrequest() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(
            tmp.path(),
            "ws/agents/withtool.md",
            &fm_file("fixture-model", &["ToolsetEcho"], "with body"),
        );
        write_md(
            tmp.path(),
            "ws/agents/without.md",
            &fm_file("fixture-model", &["Think"], "without body"),
        );
        let kernel = Kernel::new(tmp.path());
        let dispatcher = FakeDispatcher::with_toolset_tool("ToolsetEcho", "echoed");

        let mut ts1 = FakeToolset::new(vec![end_turn("ok")]);
        run_dispatch_routed(
            "dispatch",
            r#"{"path":"agents/withtool.md","query":"hi"}"#,
            &kernel,
            &mut ts1,
            &dispatcher,
            "parent",
        )
        .await
        .expect("the dispatch verb returns a tool result");
        let advertised1: Vec<String> = ts1.recorded[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert!(
            advertised1.iter().any(|n| n == "ToolsetEcho"),
            "a frontmatter-granted toolset tool is advertised to the sub-turn, got {advertised1:?}"
        );

        let mut ts2 = FakeToolset::new(vec![end_turn("ok")]);
        run_dispatch_routed(
            "dispatch",
            r#"{"path":"agents/without.md","query":"hi"}"#,
            &kernel,
            &mut ts2,
            &dispatcher,
            "parent",
        )
        .await
        .expect("the dispatch verb returns a tool result");
        let advertised2: Vec<String> = ts2.recorded[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert!(
            !advertised2.iter().any(|n| n == "ToolsetEcho"),
            "a toolset tool the frontmatter does not name is scoped out, got {advertised2:?}"
        );
    }

    // A sub-turn tool call on a frontmatter-granted TOOLSET tool is routed
    // through the tool dispatcher and its result fed back into the loop, not
    // resolved against runtime tools only and rejected as an unknown runtime
    // tool.
    #[tokio::test]
    async fn dispatch_routes_frontmatter_toolset_tool_call_through_dispatcher_and_feeds_result_back(
    ) {
        let tmp = tempfile::tempdir().unwrap();
        write_md(
            tmp.path(),
            "ws/agents/user.md",
            &fm_file("fixture-model", &["ToolsetEcho"], "user body"),
        );
        let kernel = Kernel::new(tmp.path());
        let dispatcher = FakeDispatcher::with_toolset_tool("ToolsetEcho", "echo-result");
        // Turn 1: the sub-turn model calls the granted toolset tool. Turn 2 ends.
        let call_toolset = vec![TurnEvent {
            event: Some(turn_event::Event::Complete(TurnComplete {
                stop_reason: StopReason::ToolUse as i32,
                content: vec![],
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "ToolsetEcho".into(),
                    input_json: "{}".into(),
                }],
            })),
        }];
        let mut toolset = FakeToolset::new(vec![call_toolset, end_turn("done after toolset tool")]);
        let resp = run_dispatch_routed(
            "dispatch",
            r#"{"path":"agents/user.md","query":"hi"}"#,
            &kernel,
            &mut toolset,
            &dispatcher,
            "parent",
        )
        .await
        .expect("the dispatch verb returns a tool result");

        // The toolset tool call reached the dispatcher, not the runtime arm.
        assert_eq!(
            dispatcher.routed(),
            vec!["ToolsetEcho".to_string()],
            "the sub-turn's toolset tool call is routed through the dispatcher"
        );
        // Its result is fed back into the next sub-turn round as a tool message.
        let second = &toolset.recorded[1];
        assert!(
            second
                .messages
                .iter()
                .any(|m| m.role == "tool" && content_text(&m.content).contains("echo-result")),
            "the dispatcher's tool result is fed back into the sub-turn"
        );
        assert!(!resp.is_error, "got {:?}", collect_text(&resp.content));
        assert_eq!(collect_text(&resp.content), "done after toolset tool");
    }

    // There is NO single-shot mode. A dispatched agent whose frontmatter grants
    // no toolset tools is still advertised the always-on runtime substrate
    // (read/dispatch/list/Think), exactly like the primary turn, and is
    // loop-capable: a runtime tool call is dispatched and its result fed back
    // rather than erroring or terminating.
    #[tokio::test]
    async fn dispatch_no_toolset_tools_still_advertises_runtime_substrate_and_loops() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(
            tmp.path(),
            "ws/agents/leaf.md",
            &fm_file("fixture-model", &[], "leaf body"),
        );
        let kernel = Kernel::new(tmp.path());
        // Turn 1 calls a runtime tool (`Think`); turn 2 ends. A single-shot
        // no-tools sub-agent would error on turn 1 instead of looping.
        let call_think = vec![TurnEvent {
            event: Some(turn_event::Event::Complete(TurnComplete {
                stop_reason: StopReason::ToolUse as i32,
                content: vec![],
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "Think".into(),
                    input_json: r#"{"note":"still looping"}"#.into(),
                }],
            })),
        }];
        let mut toolset = FakeToolset::new(vec![call_think, end_turn("looped to done")]);
        let dispatcher = FakeDispatcher::empty();
        let resp = run_dispatch_routed(
            "dispatch",
            r#"{"path":"agents/leaf.md","query":"hi"}"#,
            &kernel,
            &mut toolset,
            &dispatcher,
            "parent",
        )
        .await
        .expect("the dispatch verb returns a tool result");

        // The no-toolset-tools sub-request is advertised the runtime substrate.
        let advertised: Vec<String> = toolset.recorded[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        for want in ["read", "dispatch", "list", "Think"] {
            assert!(
                advertised.iter().any(|n| n == want),
                "runtime tool `{want}` is advertised to a no-toolset-tools sub-turn, got {advertised:?}"
            );
        }
        // It is loop-capable: the runtime tool call is dispatched and fed back,
        // not errored or terminated single-shot.
        assert!(
            !resp.is_error,
            "a no-toolset-tools sub-turn loops on a runtime tool call instead of erroring; got {:?}",
            collect_text(&resp.content)
        );
        assert_eq!(collect_text(&resp.content), "looped to done");
        assert_eq!(
            toolset.recorded.len(),
            2,
            "the sub-turn ran a second round after the runtime tool result, not a single shot"
        );
        assert_eq!(
            dispatcher.routed(),
            vec!["Think".to_string()],
            "the runtime tool call was routed through the dispatcher"
        );
    }

    // Subagent dispatch must not be opaque: the harness must DELIVER the
    // sub-turn's streamed frames to the gateway carrying the subagent framing
    // (parent link + the dispatched file path as the agent identifier), so the
    // client can nest them under the parent turn.
    struct CapturingRelay {
        delivered: Vec<(String, proto_common::StreamItem)>,
    }

    #[async_trait::async_trait]
    impl RelayRpc for CapturingRelay {
        async fn send_server_notification(
            &mut self,
            _channel_id: &str,
            _method: &str,
            _params_json: &str,
        ) -> Result<bool, String> {
            Ok(true)
        }
        async fn send_server_request_and_await(
            &mut self,
            _channel_id: &str,
            _request_id: &str,
            _method: &str,
            _params_json: &str,
            _timeout_seconds: u32,
        ) -> Result<crate::clients::ServerRequestOutcome, String> {
            Ok(crate::clients::ServerRequestOutcome::Result(String::new()))
        }
        async fn deliver_stream_item(
            &mut self,
            channel_id: &str,
            item: proto_common::StreamItem,
        ) -> Result<bool, String> {
            self.delivered.push((channel_id.to_string(), item));
            Ok(true)
        }
    }

    fn content_delta_then_end(text: &str, end: &str) -> Vec<TurnEvent> {
        use toolset_proto::{turn_event as te, ContentDelta};
        vec![
            TurnEvent {
                event: Some(te::Event::ContentDelta(ContentDelta { text: text.into() })),
            },
            TurnEvent {
                event: Some(turn_event::Event::Complete(TurnComplete {
                    stop_reason: StopReason::EndTurn as i32,
                    content: vec![ContentBlock {
                        block: Some(content_block::Block::Text(TextBlock { text: end.into() })),
                    }],
                    tool_calls: vec![],
                })),
            },
        ]
    }

    #[tokio::test]
    async fn dispatch_delivers_subagent_frames_with_parent_link_and_path_identity() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/agents/scout.md", "scout agent");
        let kernel = Kernel::new(tmp.path());
        let mut toolset = FakeToolset::new(vec![content_delta_then_end("looking...", "done")]);
        let dispatcher = FakeDispatcher::empty();
        let mut relay = CapturingRelay { delivered: vec![] };
        let registry = test_registry();
        let cancel = tokio_util::sync::CancellationToken::new();

        let resp = dispatch(
            "dispatch",
            r#"{"path":"agents/scout.md","query":"find it"}"#,
            &kernel,
            WS,
            &mut toolset,
            &dispatcher,
            &registry,
            "parent-conv",
            Some("reply-chan"),
            Some(&mut relay),
            &cancel,
        )
        .await
        .unwrap();

        assert!(!resp.is_error, "sub-turn should succeed: {resp:?}");
        assert!(
            !relay.delivered.is_empty(),
            "sub-turn streamed frames must be delivered to the gateway, not dropped"
        );
        let (channel, item) = relay
            .delivered
            .iter()
            .find(|(_, i)| !i.parent_conversation_id.is_empty())
            .expect("a delivered sub-turn frame must carry the parent link");
        assert_eq!(channel, "reply-chan");
        assert_eq!(
            item.parent_conversation_id, "parent-conv",
            "the delivered frame links to the parent conversation"
        );
        assert_ne!(
            item.conversation_id, "parent-conv",
            "the frame's own conversation is the minted child, distinct from the parent"
        );
        assert_eq!(
            item.agent_name, "agents/scout.md",
            "the delivered frame carries the dispatched file path as the sub-agent identifier"
        );
    }

    #[tokio::test]
    async fn dispatch_with_granted_tools_loops_on_tool_use_instead_of_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(
            tmp.path(),
            "ws/agents/looper.md",
            &fm_file("fixture-model", &["Think"], "looper body"),
        );
        let kernel = Kernel::new(tmp.path());
        // Turn 1 calls the granted Think tool; turn 2 ends. A correct loop runs
        // both; the old error arm stops after turn 1 with is_error.
        let call_think = vec![TurnEvent {
            event: Some(turn_event::Event::Complete(TurnComplete {
                stop_reason: StopReason::ToolUse as i32,
                content: vec![],
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "Think".into(),
                    input_json: r#"{"note":"looping"}"#.into(),
                }],
            })),
        }];
        let mut toolset = FakeToolset::new(vec![call_think, end_turn("looped to done")]);
        let resp = run_dispatch(
            "dispatch",
            r#"{"path":"agents/looper.md","query":"hi"}"#,
            &kernel,
            &mut toolset,
            "parent",
        )
        .await
        .expect("the dispatch verb returns a tool result once implemented");
        assert!(
            !resp.is_error,
            "a granted-tools sub-turn loops through ToolUse instead of erroring; got {:?}",
            collect_text(&resp.content)
        );
        assert_eq!(collect_text(&resp.content), "looped to done");
        assert_eq!(
            toolset.recorded.len(),
            2,
            "the sub-turn ran a second round after the tool result, not a single shot"
        );
    }

    #[tokio::test]
    async fn dispatch_think_echoes_note() {
        let (_tmp, kernel) = empty_kernel();
        let mut toolset = FakeToolset::empty();
        let resp = run_dispatch(
            "Think",
            r#"{"note":"file 1 looks like an assignation"}"#,
            &kernel,
            &mut toolset,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert!(collect_text(&resp.content).contains("file 1 looks like an assignation"));
        assert!(collect_text(&resp.content).starts_with("noted:"));
        // Crucially: no toolset calls were made — this is a purely in-process tool.
        assert!(toolset.recorded.is_empty());
    }

    #[tokio::test]
    async fn dispatch_think_invalid_json_returns_is_error() {
        let (_tmp, kernel) = empty_kernel();
        let mut toolset = FakeToolset::empty();
        let resp = run_dispatch("Think", "{not json}", &kernel, &mut toolset, "parent")
            .await
            .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("invalid arguments"));
    }

    // `read` returns a nested file's body straight from the mounted kernel
    // volume — a pure local filesystem read, no LLM dispatch.
    #[tokio::test]
    async fn dispatch_read_returns_nested_file_body() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/skills/foo/SKILL.md", "skill body");
        let kernel = Kernel::new(tmp.path());
        let mut toolset = FakeToolset::empty();
        let resp = run_dispatch(
            "read",
            r#"{"path":"skills/foo/SKILL.md"}"#,
            &kernel,
            &mut toolset,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert_eq!(collect_text(&resp.content), "skill body");
        assert!(toolset.recorded.is_empty());
    }

    #[tokio::test]
    async fn dispatch_read_missing_returns_is_error() {
        let (_tmp, kernel) = empty_kernel();
        let mut toolset = FakeToolset::empty();
        let resp = run_dispatch(
            "read",
            r#"{"path":"nope.md"}"#,
            &kernel,
            &mut toolset,
            "parent",
        )
        .await
        .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("not found"));
    }

    // `list` returns the tree's flat sorted paths as a JSON array — a pure
    // kernel read, no LLM dispatch.
    #[tokio::test]
    async fn dispatch_list_returns_flat_sorted_paths() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/AGENTS.md", "root");
        write_md(tmp.path(), "ws/skills/beta.md", "b");
        write_md(tmp.path(), "ws/agents/alpha.md", "a");
        let kernel = Kernel::new(tmp.path());
        let mut toolset = FakeToolset::empty();
        let resp = run_dispatch("list", "{}", &kernel, &mut toolset, "parent")
            .await
            .unwrap();
        assert!(!resp.is_error);
        let paths: Vec<String> = serde_json::from_str(&collect_text(&resp.content)).unwrap();
        assert_eq!(
            paths,
            vec!["AGENTS.md", "agents/alpha.md", "skills/beta.md"]
        );
        assert!(toolset.recorded.is_empty());
    }

    #[tokio::test]
    async fn dispatch_list_detail_returns_name_and_description_json() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(
            tmp.path(),
            "ws/skills/classify.md",
            "# Classify\n\nDecide the doctype and date.\n",
        );
        let kernel = Kernel::new(tmp.path());
        let mut toolset = FakeToolset::empty();
        let resp = run_dispatch(
            "list",
            r#"{"detail":true}"#,
            &kernel,
            &mut toolset,
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
        let (_tmp, kernel) = empty_kernel();
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

        let mut toolset = FakeToolset::empty();
        let dispatcher = FakeDispatcher::empty();
        let cancel = tokio_util::sync::CancellationToken::new();
        let resp = dispatch(
            "RecentTurns",
            r#"{"limit":5}"#,
            &kernel,
            WS,
            &mut toolset,
            &dispatcher,
            &registry,
            &id,
            None,
            None,
            &cancel,
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
        assert!(toolset.recorded.is_empty());
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
                #[serde(default)]
                tag: Option<String>,
            }
            let h = Helper::deserialize(deserializer)?;
            Ok(RecentTurnJson {
                seq: h.seq,
                ts: h.ts,
                role: h.role,
                text: h.text,
                tag: h.tag,
            })
        }
    }

    #[tokio::test]
    async fn dispatch_unknown_runtime_tool_returns_err() {
        let (_tmp, kernel) = empty_kernel();
        let mut toolset = FakeToolset::empty();
        let err = run_dispatch("Ghost", "{}", &kernel, &mut toolset, "parent")
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchAbort::Error(ref e) if e.contains("unknown runtime tool")));
    }
}
