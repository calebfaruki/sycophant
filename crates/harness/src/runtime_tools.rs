//! Harness-local runtime tools: `Agent`, `Agents`, `Skill`, `Skills`,
//! `Think`, and `RecentTurns`.
//!
//! These are framework-defined tools the LLM can call. The harness
//! advertises them alongside the airlock-served chamber tools and
//! dispatches them in-process. Persona and skill content is read directly
//! from this workspace's mounted kernel volume; `Agent` also composes a
//! hangar-ctrl round-trip. They never fabricate results.
//!
//! `Agent(name, query)` is single-shot: load the named sub-agent's
//! persona, submit one `Turn` to hangar with that as system prompt
//! and the query as user content, return the assistant text. No nested
//! tool-use loop inside the sub-conversation; the sub-agent's turn is a
//! single round-trip.
//!
//! `Agents()` returns `[{name, description}, ...]` enumerated from the
//! kernel. `Skill(name)` returns a skill file's markdown; `Skills()`
//! lists the workspace's skills.

use hangar_proto::{Message, StopReason, TurnRequest, TurnRole};
use hangar_providers::types::content_text;
use proto_common::{CallToolResponse, ToolInfo};
use serde::{Deserialize, Serialize};

use crate::agent::{collect_text, text_block};
use crate::clients::{HangarRpc, RelayRpc};
use crate::kernel::{first_paragraph, Kernel, KernelError};
use crate::registry::ConversationRegistry;
use crate::turn;

pub(crate) const AGENT_TOOL_NAME: &str = "Agent";
pub(crate) const AGENTS_TOOL_NAME: &str = "Agents";
pub(crate) const SKILL_TOOL_NAME: &str = "Skill";
pub(crate) const SKILLS_TOOL_NAME: &str = "Skills";
pub(crate) const THINK_TOOL_NAME: &str = "Think";
pub(crate) const RECENT_TURNS_TOOL_NAME: &str = "RecentTurns";

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
            name: AGENT_TOOL_NAME.into(),
            description: "Invoke a sub-agent: load the named persona from the workspace kernel and \
                          submit the query to the LLM with that persona as the system prompt. \
                          Returns the sub-agent's response text. Single round-trip — the sub-agent \
                          does not have its own tool-use loop."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Sub-agent name (basename of agents/<name>.md in the workspace kernel)."
                    },
                    "query": {
                        "type": "string",
                        "description": "The question or instruction to send to the sub-agent."
                    }
                },
                "required": ["name", "query"]
            })
            .to_string(),
        },
        ToolInfo {
            name: AGENTS_TOOL_NAME.into(),
            description: "List the available sub-agents in this workspace along with each one's \
                          description. Use this to discover what specialists you can delegate to."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            })
            .to_string(),
        },
        ToolInfo {
            name: SKILL_TOOL_NAME.into(),
            description: "Read a skill file from the workspace kernel and return its markdown \
                          contents. Skills are operator-authored procedures the agent can follow."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Skill name (basename of skills/<name>.md, without the .md extension)."
                    }
                },
                "required": ["name"]
            })
            .to_string(),
        },
        ToolInfo {
            name: SKILLS_TOOL_NAME.into(),
            description: "List the names of skills available in the current workspace.".into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            })
            .to_string(),
        },
        ToolInfo {
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
struct AgentArgs {
    name: String,
    query: String,
}

#[derive(Deserialize)]
struct SkillArgs {
    name: String,
}

#[derive(Deserialize, Default)]
struct SkillsArgs {
    /// When set, return `[{name, description}]` instead of bare names.
    /// The client's command menu sets it; the LLM's advertised schema
    /// omits it, so the default stays a names array.
    #[serde(default)]
    detail: bool,
}

#[derive(Serialize)]
struct SkillInfo {
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

#[derive(Serialize)]
struct AgentInfoJson {
    name: String,
    description: String,
}

/// Entrypoint used by `ToolRouter::call_tool` for `Runtime`-source tools.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch(
    name: &str,
    input_json: &str,
    kernel: &Kernel,
    workspace: &str,
    hangar: &mut dyn HangarRpc,
    registry: &ConversationRegistry,
    parent_conversation_id: &str,
    reply_channel: Option<&str>,
    relay: Option<&mut dyn RelayRpc>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<CallToolResponse, DispatchAbort> {
    match name {
        AGENT_TOOL_NAME => {
            dispatch_agent(
                input_json,
                kernel,
                workspace,
                hangar,
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
                    content: vec![text_block(format!("Agent error: {e}"))],
                    is_error: true,
                }),
                // A cancelled sub-agent is terminal — it must NOT fold into an
                // is_error result the loop continues on. Propagate it.
                DispatchAbort::Cancelled => Err(DispatchAbort::Cancelled),
            })
        }
        SKILL_TOOL_NAME => dispatch_skill(input_json, kernel, workspace),
        SKILLS_TOOL_NAME => dispatch_skills(input_json, kernel, workspace),
        AGENTS_TOOL_NAME => dispatch_agents(kernel, workspace).await.or_else(|e| {
            Ok(CallToolResponse {
                content: vec![text_block(format!("Agents error: {e}"))],
                is_error: true,
            })
        }),
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
            text: content_text(&e.message.content).unwrap_or_default().into(),
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

#[allow(clippy::too_many_arguments)]
async fn dispatch_agent(
    input_json: &str,
    kernel: &Kernel,
    workspace: &str,
    hangar: &mut dyn HangarRpc,
    registry: &ConversationRegistry,
    parent_conversation_id: &str,
    reply_channel: Option<&str>,
    relay: Option<&mut dyn RelayRpc>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<CallToolResponse, DispatchAbort> {
    let args: AgentArgs = serde_json::from_str(input_json)
        .map_err(|e| DispatchAbort::Error(format!("invalid Agent arguments: {e}")))?;

    // Empty `name` is the "primary AGENTS.md" convention, used by the
    // orchestrator's per-turn read. Allowing it here would silently
    // self-dispatch the orchestrator into a no-tool sub-conversation.
    if args.name.is_empty() {
        return Err(DispatchAbort::Error(
            "agent name cannot be empty; call Agents() to list available sub-agents".into(),
        ));
    }

    let persona = kernel
        .read_agent(workspace, &args.name)
        .map_err(|e| DispatchAbort::Error(kernel_agent_error(&args.name, e)))?;

    // Sub-conversation linked to the parent so logs can be correlated.
    // `correlation_id` carries the parent's id; hangar-controller
    // stamps the relationship onto the log entries.
    let sub_request = TurnRequest {
        system: Some(persona),
        tools: vec![],
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
        correlation_id: Some(parent_conversation_id.to_string()),
        // Sub-conversation id minted locally — the harness owns minting.
        // The child id is minted but never `register_turn`'d: the sub-agent
        // shares the parent turn's `cancel` token, not a second registration.
        conversation_id: registry.mint().await.map_err(DispatchAbort::Error)?,
    };

    let child_conversation_id = sub_request.conversation_id.clone();
    let mut stream = hangar
        .turn(sub_request)
        .await
        .map_err(DispatchAbort::Error)?;
    // Sub-agent frames carry the child's own id plus the parent link so the
    // client groups them under their parent turn. A live GatewaySink relays
    // them when the turn has a reply channel; otherwise they drop.
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
    let mut emit = turn::EmitState::new_subagent(
        child_conversation_id,
        parent_conversation_id.to_string(),
        args.name.clone(),
    );
    let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_SUBAGENT_SCRUB__");
    // The sub-agent shares the parent turn's cancellation token: a fired parent
    // cancel abandons this stream at the next event boundary and surfaces as a
    // terminal `DispatchAbort::Cancelled` rather than draining to natural end.
    let outcome = turn::consume_turn_stream_cancellable(
        &mut *stream,
        turn::DEFAULT_IDLE_GAP,
        sink,
        &mut emit,
        &scrub,
        cancel,
    )
    .await
    .map_err(|abort| match abort {
        turn::TurnAbort::Ended(e) => DispatchAbort::Error(e),
        turn::TurnAbort::Cancelled => DispatchAbort::Cancelled,
    })?;

    let text = collect_text(&outcome.content);
    match outcome.stop_reason {
        StopReason::EndTurn | StopReason::MaxTokens => Ok(CallToolResponse {
            content: vec![text_block(text)],
            is_error: false,
        }),
        StopReason::ToolUse => {
            // Sub-agent has no tools and shouldn't try to call any. If it
            // does, surface as an LLM-visible error so the orchestrator
            // can rephrase the delegation. Hangar already wrote the
            // partial turn to the log.
            Ok(CallToolResponse {
                content: vec![text_block(format!(
                    "sub-agent attempted a tool call (no tools available); partial text: {text}"
                ))],
                is_error: true,
            })
        }
        other => Ok(CallToolResponse {
            content: vec![text_block(format!(
                "sub-agent stopped unexpectedly ({:?}): {text}",
                other
            ))],
            is_error: true,
        }),
    }
}

async fn dispatch_agents(kernel: &Kernel, workspace: &str) -> Result<CallToolResponse, String> {
    let names = kernel
        .list_agents(workspace)
        .map_err(|e| format!("list agents failed: {e}"))?;
    let mut projected = Vec::with_capacity(names.len());
    for name in names {
        // Best-effort description: a name whose file vanished mid-enumeration
        // is skipped, not fatal.
        if let Ok(body) = kernel.read_agent(workspace, &name) {
            projected.push(AgentInfoJson {
                name,
                description: first_paragraph(&body),
            });
        }
    }
    let output =
        serde_json::to_string(&projected).map_err(|e| format!("serialize Agents output: {e}"))?;
    Ok(CallToolResponse {
        content: vec![text_block(output)],
        is_error: false,
    })
}

/// Map a sub-agent persona read error to an LLM-visible message that names the
/// requested agent. `Io` is an infrastructure failure surfaced verbatim.
fn kernel_agent_error(name: &str, e: KernelError) -> String {
    match e {
        KernelError::NotFound => format!("sub-agent persona not found: {name}"),
        KernelError::InvalidName(n) => format!("invalid sub-agent name: {n}"),
        KernelError::PathEscape => "sub-agent path escapes workspace root".into(),
        KernelError::Io(io) => format!("io error: {io}"),
    }
}

/// `Skill(name)`: read a skill file's markdown from the workspace kernel.
/// A missing/invalid/escaping skill folds into an `is_error` tool result the
/// LLM sees; an I/O failure is a true infra abort.
fn dispatch_skill(
    input_json: &str,
    kernel: &Kernel,
    workspace: &str,
) -> Result<CallToolResponse, DispatchAbort> {
    let args: SkillArgs = serde_json::from_str(input_json)
        .map_err(|e| DispatchAbort::Error(format!("invalid Skill arguments: {e}")))?;
    match kernel.read_skill(workspace, &args.name) {
        Ok(content) => Ok(CallToolResponse {
            content: vec![text_block(content)],
            is_error: false,
        }),
        Err(KernelError::NotFound) => Ok(CallToolResponse {
            content: vec![text_block(format!("skill not found: {}", args.name))],
            is_error: true,
        }),
        Err(KernelError::InvalidName(n)) => Ok(CallToolResponse {
            content: vec![text_block(format!("invalid skill name: {n}"))],
            is_error: true,
        }),
        Err(KernelError::PathEscape) => Ok(CallToolResponse {
            content: vec![text_block("skill path escapes workspace root".into())],
            is_error: true,
        }),
        Err(KernelError::Io(e)) => Err(DispatchAbort::Error(format!("io error: {e}"))),
    }
}

/// `Skills()`: list the workspace's skill names. `{detail:true}` returns
/// `[{name, description}]` (first-paragraph descriptions); the default returns
/// a bare names array.
fn dispatch_skills(
    input_json: &str,
    kernel: &Kernel,
    workspace: &str,
) -> Result<CallToolResponse, DispatchAbort> {
    // Empty input_json is the historical "no args" form; treat it as `{}`.
    let args: SkillsArgs = if input_json.trim().is_empty() {
        SkillsArgs::default()
    } else {
        serde_json::from_str(input_json)
            .map_err(|e| DispatchAbort::Error(format!("invalid Skills arguments: {e}")))?
    };
    let names = kernel
        .list_skills(workspace)
        .map_err(|e| DispatchAbort::Error(format!("list skills failed: {e}")))?;
    let json = if args.detail {
        let mut infos = Vec::with_capacity(names.len());
        for name in names {
            // Best-effort description (mirror list_agents): a name whose file
            // vanished mid-enumeration is skipped, not fatal.
            if let Ok(body) = kernel.read_skill(workspace, &name) {
                infos.push(SkillInfo {
                    name,
                    description: first_paragraph(&body),
                });
            }
        }
        serde_json::to_string(&infos)
            .map_err(|e| DispatchAbort::Error(format!("serialize: {e}")))?
    } else {
        serde_json::to_string(&names)
            .map_err(|e| DispatchAbort::Error(format!("serialize: {e}")))?
    };
    Ok(CallToolResponse {
        content: vec![text_block(json)],
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::TurnSource;
    use crate::kernel::Kernel;
    use crate::test_doubles::EndlessHangar;
    use hangar_proto::{
        content_block, turn_event, ContentBlock, TextBlock, TurnComplete, TurnEvent,
    };
    use std::collections::VecDeque;
    use std::path::Path;
    use tempfile::TempDir;

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

    struct FakeHangar {
        turns: VecDeque<Vec<TurnEvent>>,
        recorded: Vec<TurnRequest>,
    }

    impl FakeHangar {
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
    impl HangarRpc for FakeHangar {
        async fn turn(&mut self, request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
            self.recorded.push(request);
            let events = self
                .turns
                .pop_front()
                .ok_or_else(|| "FakeHangar: no more scripted turns".to_string())?;
            Ok(Box::new(FakeTurnSource {
                events: events.into(),
            }))
        }
        async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn test_registry() -> ConversationRegistry {
        use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
        let root = tempfile::TempDir::new().unwrap().keep();
        let factory: std::sync::Arc<dyn ConversationStoreFactory> =
            std::sync::Arc::new(LocalFsFactory::new(root));
        ConversationRegistry::new(factory)
    }

    /// Dispatch against an in-process kernel with a throwaway registry.
    async fn run_dispatch(
        name: &str,
        input: &str,
        kernel: &Kernel,
        hangar: &mut FakeHangar,
        parent: &str,
    ) -> Result<CallToolResponse, DispatchAbort> {
        let registry = test_registry();
        let cancel = tokio_util::sync::CancellationToken::new();
        dispatch(
            name, input, kernel, WS, hangar, &registry, parent, None, None, &cancel,
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

    fn tool_use(text: &str) -> Vec<TurnEvent> {
        complete(StopReason::ToolUse, text)
    }

    #[test]
    fn tool_definitions_includes_runtime_and_kernel_tools() {
        let tools = tool_definitions();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Agent"));
        assert!(names.contains(&"Agents"));
        assert!(names.contains(&"Think"));
        // Skill/Skills are framework runtime tools served in-process from the
        // kernel, advertised statically here.
        assert!(names.contains(&"Skill"));
        assert!(names.contains(&"Skills"));
    }

    #[tokio::test]
    async fn dispatch_think_echoes_note() {
        let (_tmp, kernel) = empty_kernel();
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch(
            "Think",
            r#"{"note":"file 1 looks like an assignation"}"#,
            &kernel,
            &mut hangar,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert!(collect_text(&resp.content).contains("file 1 looks like an assignation"));
        assert!(collect_text(&resp.content).starts_with("noted:"));
        // Crucially: no hangar calls were made — this is a purely in-process tool.
        assert!(hangar.recorded.is_empty());
    }

    #[tokio::test]
    async fn dispatch_think_invalid_json_returns_is_error() {
        let (_tmp, kernel) = empty_kernel();
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch("Think", "{not json}", &kernel, &mut hangar, "parent")
            .await
            .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("invalid arguments"));
    }

    // Skill/Skills resolve from the mounted kernel volume in-process — no
    // network round trip to a separate kernel-serving pod (there is no such
    // client left to call). The reader is a temp-dir filesystem read.
    #[tokio::test]
    async fn dispatch_skill_returns_markdown_from_the_kernel() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/skills/classify.md", "classify body");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch(
            "Skill",
            r#"{"name":"classify"}"#,
            &kernel,
            &mut hangar,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert_eq!(collect_text(&resp.content), "classify body");
        // No LLM dispatch: a skill read is a pure local file read.
        assert!(hangar.recorded.is_empty());
    }

    #[tokio::test]
    async fn dispatch_skill_missing_returns_is_error() {
        let (_tmp, kernel) = empty_kernel();
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch(
            "Skill",
            r#"{"name":"missing"}"#,
            &kernel,
            &mut hangar,
            "parent",
        )
        .await
        .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("not found"));
    }

    // A Skill text answer must arrive as a one-element content list whose single
    // part is a TEXT part carrying the body — not a bare string, not an empty
    // list, not an image part.
    #[tokio::test]
    async fn dispatch_skill_answer_is_a_single_text_part() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/skills/classify.md", "classify body");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch(
            "Skill",
            r#"{"name":"classify"}"#,
            &kernel,
            &mut hangar,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert_eq!(
            resp.content.len(),
            1,
            "a text answer is represented as a one-part content list"
        );
        match resp.content[0].block.as_ref() {
            Some(proto_common::content_block::Block::Text(t)) => {
                assert_eq!(t.text, "classify body", "the text part carries the body");
            }
            other => panic!("a text answer's single part must be a text part, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_skills_returns_sorted_names_json() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/skills/beta.md", "b");
        write_md(tmp.path(), "ws/skills/alpha.md", "a");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch("Skills", "{}", &kernel, &mut hangar, "parent")
            .await
            .unwrap();
        assert!(!resp.is_error);
        let names: Vec<String> = serde_json::from_str(&collect_text(&resp.content)).unwrap();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[tokio::test]
    async fn dispatch_skills_detail_returns_name_and_description_json() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(
            tmp.path(),
            "ws/skills/classify.md",
            "# Classify\n\nDecide the doctype and date.\n\n## Procedure\n1. look\n",
        );
        write_md(
            tmp.path(),
            "ws/skills/survey.md",
            "# Survey\n\nWalk the tree.\n",
        );
        let kernel = Kernel::new(tmp.path());
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch(
            "Skills",
            r#"{"detail":true}"#,
            &kernel,
            &mut hangar,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        let infos: Vec<serde_json::Value> =
            serde_json::from_str(&collect_text(&resp.content)).unwrap();
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0]["name"], "classify");
        assert_eq!(infos[0]["description"], "Decide the doctype and date.");
        assert_eq!(infos[1]["name"], "survey");
        assert_eq!(infos[1]["description"], "Walk the tree.");
    }

    #[tokio::test]
    async fn dispatch_agent_reads_persona_from_kernel_and_dispatches_then_returns_text() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/agents/alice.md", "alice persona");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = FakeHangar::new(vec![end_turn("alice says hello")]);
        let resp = run_dispatch(
            "Agent",
            r#"{"name":"alice","query":"hi"}"#,
            &kernel,
            &mut hangar,
            "parent-conv",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert_eq!(collect_text(&resp.content), "alice says hello");
        assert_eq!(hangar.recorded.len(), 1);
        let sent = &hangar.recorded[0];
        // The sub-agent's system prompt is the persona read straight from the
        // kernel volume — no gRPC persona fetch.
        assert_eq!(sent.system.as_deref(), Some("alice persona"));
        assert!(!sent.conversation_id.is_empty());
        assert_ne!(sent.conversation_id, "parent-conv");
        assert_eq!(sent.correlation_id.as_deref(), Some("parent-conv"));
        assert_eq!(sent.role, Some(TurnRole::Delegate as i32));
    }

    #[tokio::test]
    async fn dispatch_agent_missing_persona_returns_is_error() {
        let (_tmp, kernel) = empty_kernel();
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch(
            "Agent",
            r#"{"name":"ghost","query":"hi"}"#,
            &kernel,
            &mut hangar,
            "parent",
        )
        .await
        .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("ghost"));
        // A missing persona must not open a sub-conversation.
        assert!(hangar.recorded.is_empty());
    }

    #[tokio::test]
    async fn dispatch_agent_subagent_tool_use_stop_returns_is_error() {
        // Sub-agents run a single round-trip with no tools. A sub-agent that
        // stops on ToolUse must surface as an LLM-visible error. Mutant: fold
        // ToolUse into the EndTurn|MaxTokens success arm -> is_error false, red.
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/agents/helper.md", "helper persona");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = FakeHangar::new(vec![tool_use("partial")]);
        let resp = run_dispatch(
            "Agent",
            r#"{"name":"helper","query":"hi"}"#,
            &kernel,
            &mut hangar,
            "parent",
        )
        .await
        .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("attempted a tool call"));
    }

    #[tokio::test]
    async fn dispatch_agent_empty_name_returns_is_error() {
        // Empty `name` is the "primary AGENTS.md" convention; letting it through
        // would silently self-dispatch the orchestrator. Must reject without any
        // kernel read or dispatch.
        let (_tmp, kernel) = empty_kernel();
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch(
            "Agent",
            r#"{"name":"","query":"hi"}"#,
            &kernel,
            &mut hangar,
            "parent",
        )
        .await
        .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("name cannot be empty"));
        assert!(
            hangar.recorded.is_empty(),
            "no sub-conversation should be minted",
        );
    }

    #[tokio::test]
    async fn dispatch_agent_invalid_json_returns_is_error() {
        let (_tmp, kernel) = empty_kernel();
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch("Agent", "{not json}", &kernel, &mut hangar, "parent")
            .await
            .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("invalid Agent arguments"));
    }

    #[tokio::test]
    async fn dispatch_agents_returns_projected_json() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/agents/alice.md", "# Alice\n\nlegal\n");
        write_md(tmp.path(), "ws/agents/bob.md", "# Bob\n\nops\n");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = FakeHangar::empty();
        let resp = run_dispatch("Agents", "{}", &kernel, &mut hangar, "parent")
            .await
            .unwrap();
        assert!(!resp.is_error);
        let parsed: Vec<AgentInfoJson> =
            serde_json::from_str(&collect_text(&resp.content)).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "alice");
        assert_eq!(parsed[0].description, "legal");
        assert_eq!(parsed[1].name, "bob");
        assert_eq!(parsed[1].description, "ops");
        // Listing is a pure kernel read: no LLM dispatch.
        assert!(hangar.recorded.is_empty());
    }

    #[tokio::test]
    async fn recent_turns_reads_log_tail_without_mutating() {
        use hangar_providers::types::{ContentBlock, Message};
        let (_tmp, kernel) = empty_kernel();
        let registry = test_registry();
        let id = registry.mint().await.unwrap();
        let log = registry.get_or_create(&id).await.unwrap();
        log.write()
            .await
            .append(Message {
                role: "user".into(),
                content: Some(ContentBlock::text_content("first")),
                tool_calls: None,
                tool_call_id: None,
                is_error: None,
            })
            .await
            .unwrap();

        let mut hangar = FakeHangar::empty();
        let cancel = tokio_util::sync::CancellationToken::new();
        let resp = dispatch(
            "RecentTurns",
            r#"{"limit":5}"#,
            &kernel,
            WS,
            &mut hangar,
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
        assert!(hangar.recorded.is_empty());
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
        let mut hangar = FakeHangar::empty();
        let err = run_dispatch("Ghost", "{}", &kernel, &mut hangar, "parent")
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchAbort::Error(ref e) if e.contains("unknown runtime tool")));
    }

    impl<'de> Deserialize<'de> for AgentInfoJson {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            #[derive(Deserialize)]
            struct Helper {
                name: String,
                description: String,
            }
            let h = Helper::deserialize(deserializer)?;
            Ok(AgentInfoJson {
                name: h.name,
                description: h.description,
            })
        }
    }

    // Subagent dispatch must not be opaque: the harness must DELIVER the
    // sub-agent's streamed frames to the gateway instead of dropping them.

    use hangar_proto::{turn_event as te, ContentDelta};
    use proto_common::StreamItem;

    fn content_delta_then_end(text: &str, end: &str) -> Vec<TurnEvent> {
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

    struct CapturingRelay {
        delivered: Vec<(String, StreamItem)>,
    }

    #[async_trait::async_trait]
    impl crate::clients::RelayRpc for CapturingRelay {
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
            item: StreamItem,
        ) -> Result<bool, String> {
            self.delivered.push((channel_id.to_string(), item));
            Ok(true)
        }
    }

    #[tokio::test]
    async fn dispatch_agent_delivers_subagent_frames_to_gateway() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/agents/scout.md", "scout persona");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = FakeHangar::new(vec![content_delta_then_end("looking...", "done")]);
        let mut relay = CapturingRelay { delivered: vec![] };
        let registry = test_registry();
        let cancel = tokio_util::sync::CancellationToken::new();

        let resp = dispatch_agent(
            r#"{"name":"scout","query":"find it"}"#,
            &kernel,
            WS,
            &mut hangar,
            &registry,
            "parent-conv",
            Some("reply-chan"),
            Some(&mut relay),
            &cancel,
        )
        .await
        .unwrap();

        assert!(!resp.is_error, "sub-agent turn should succeed: {resp:?}");
        assert!(
            !relay.delivered.is_empty(),
            "sub-agent streamed frames must be delivered to the gateway, not dropped"
        );
        let (channel, item) = relay
            .delivered
            .iter()
            .find(|(_, i)| !i.parent_conversation_id.is_empty())
            .expect("a delivered sub-agent frame must carry the parent link");
        assert_eq!(channel, "reply-chan");
        assert_eq!(item.parent_conversation_id, "parent-conv");
        assert_ne!(
            item.conversation_id, "parent-conv",
            "the frame's own conversation is the child, distinct from the parent"
        );
        assert_eq!(
            item.agent_name, "scout",
            "the delivered frame must carry the dispatched agent's name"
        );
    }

    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn dispatch_agent_cancels_subagent_stream_instead_of_draining() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/agents/scout.md", "scout persona");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = EndlessHangar;
        let registry = test_registry();
        let cancel = CancellationToken::new();
        cancel.cancel(); // fired before the first poll

        let outcome = dispatch_agent(
            r#"{"name":"scout","query":"go"}"#,
            &kernel,
            WS,
            &mut hangar,
            &registry,
            "parent-conv",
            None,
            None,
            &cancel,
        )
        .await;

        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "a fired parent cancel must abandon the sub-agent as Cancelled, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn dispatch_agent_uncancelled_runs_to_normal_completion() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/agents/scout.md", "scout persona");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = FakeHangar::new(vec![end_turn("scout says hi")]);
        let registry = test_registry();
        let cancel = CancellationToken::new(); // never fired

        let resp = dispatch_agent(
            r#"{"name":"scout","query":"hi"}"#,
            &kernel,
            WS,
            &mut hangar,
            &registry,
            "parent-conv",
            None,
            None,
            &cancel,
        )
        .await
        .expect("an uncancelled sub-agent must complete normally, not abort");

        assert!(!resp.is_error);
        assert_eq!(collect_text(&resp.content), "scout says hi");
    }

    #[tokio::test]
    async fn dispatch_forwards_cancel_to_the_agent_arm() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/agents/scout.md", "scout persona");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = EndlessHangar;
        let registry = test_registry();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = dispatch(
            "Agent",
            r#"{"name":"scout","query":"go"}"#,
            &kernel,
            WS,
            &mut hangar,
            &registry,
            "parent-conv",
            None,
            None,
            &cancel,
        )
        .await;

        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "dispatch must forward the fired cancel into the Agent arm, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn dispatch_agent_does_not_register_a_second_turn_for_the_child() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws/agents/scout.md", "scout persona");
        let kernel = Kernel::new(tmp.path());
        let mut hangar = FakeHangar::new(vec![end_turn("done")]);
        let registry = test_registry();
        let cancel = CancellationToken::new();

        let _ = dispatch_agent(
            r#"{"name":"scout","query":"hi"}"#,
            &kernel,
            WS,
            &mut hangar,
            &registry,
            "parent-conv",
            None,
            None,
            &cancel,
        )
        .await
        .expect("sub-agent completes normally");

        let child_id = hangar.recorded[0].conversation_id.clone();
        assert_ne!(child_id, "parent-conv");
        assert!(
            !registry.cancel(&child_id).await,
            "the child sub-conversation must NOT be a registered (independently cancellable) turn"
        );
    }
}
