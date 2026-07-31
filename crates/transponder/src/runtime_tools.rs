//! Transponder-local runtime tools: `Agent` and `Agents`.
//!
//! These are framework-defined tools the LLM can call. The transponder
//! advertises them alongside the controller-served Skills/Skills/chamber
//! tools, and dispatches them in-process. The implementations compose
//! authoritative controller calls — mainframe-ctrl for persona content,
//! hangar-ctrl for the LLM round-trip on `Agent` — and never fabricate
//! results.
//!
//! `Agent(name, query)` is single-shot: load the named sub-agent's
//! persona, submit one `Turn` to hangar with that as system prompt
//! and the query as user content, return the assistant text. No nested
//! tool-use loop inside the sub-conversation; the sub-agent's turn is a
//! single round-trip.
//!
//! `Agents()` returns `[{name, description}, ...]` enumerated from the
//! mainframe.

use hangar_proto::{Message, StopReason, TurnRequest, TurnRole};
use hangar_providers::types::content_text;
use proto_common::{CallToolResponse, ToolInfo};
use serde::{Deserialize, Serialize};

use crate::agent::{collect_text, text_block};
use crate::clients::{HangarRpc, MainframeRpc, RelayRpc};
use crate::registry::ConversationRegistry;
use crate::turn;

pub(crate) const AGENT_TOOL_NAME: &str = "Agent";
pub(crate) const AGENTS_TOOL_NAME: &str = "Agents";
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
            description: "Invoke a sub-agent: load the named persona from the mainframe and \
                          submit the query to the LLM with that persona as the system prompt. \
                          Returns the sub-agent's response text. Single round-trip — the sub-agent \
                          does not have its own tool-use loop."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Sub-agent name (basename of agents/<name>.md in the mainframe)."
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
    mainframe: &mut dyn MainframeRpc,
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
                mainframe,
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
        AGENTS_TOOL_NAME => dispatch_agents(mainframe).await.or_else(|e| {
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
    mainframe: &mut dyn MainframeRpc,
    hangar: &mut dyn HangarRpc,
    registry: &ConversationRegistry,
    parent_conversation_id: &str,
    reply_channel: Option<&str>,
    relay: Option<&mut dyn RelayRpc>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<CallToolResponse, DispatchAbort> {
    let args: AgentArgs = serde_json::from_str(input_json)
        .map_err(|e| DispatchAbort::Error(format!("invalid Agent arguments: {e}")))?;

    // Empty `name` is mainframe's convention for "primary AGENTS.md", used
    // by the orchestrator's per-turn fetch. Allowing it here would silently
    // self-dispatch the orchestrator into a no-tool sub-conversation.
    if args.name.is_empty() {
        return Err(DispatchAbort::Error(
            "agent name cannot be empty; call Agents() to list available sub-agents".into(),
        ));
    }

    let persona = mainframe
        .get_agent(&args.name)
        .await
        .map_err(DispatchAbort::Error)?;

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
        // Sub-conversation id minted locally — the transponder owns minting.
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

async fn dispatch_agents(mainframe: &mut dyn MainframeRpc) -> Result<CallToolResponse, String> {
    let agents = mainframe.list_agents().await?;
    let projected: Vec<AgentInfoJson> = agents
        .into_iter()
        .map(|a| AgentInfoJson {
            name: a.name,
            description: a.description,
        })
        .collect();
    let output =
        serde_json::to_string(&projected).map_err(|e| format!("serialize Agents output: {e}"))?;
    Ok(CallToolResponse {
        content: vec![text_block(output)],
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::TurnSource;
    use crate::test_doubles::{EndlessHangar, FakeMainframe};
    use hangar_proto::{
        content_block, turn_event, ContentBlock, TextBlock, TurnComplete, TurnEvent,
    };
    use mainframe_proto::AgentInfo;
    use std::collections::VecDeque;

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

    /// Dispatch with a throwaway registry — the default for these tests.
    async fn run_dispatch(
        name: &str,
        input: &str,
        mainframe: &mut FakeMainframe,
        hangar: &mut FakeHangar,
        parent: &str,
    ) -> Result<CallToolResponse, DispatchAbort> {
        let registry = test_registry();
        let cancel = tokio_util::sync::CancellationToken::new();
        dispatch(
            name, input, mainframe, hangar, &registry, parent, None, None, &cancel,
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
    fn tool_definitions_includes_agent_and_agents() {
        let tools = tool_definitions();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Agent"));
        assert!(names.contains(&"Agents"));
        assert!(names.contains(&"Think"));
    }

    #[tokio::test]
    async fn dispatch_think_echoes_note() {
        let mut mainframe = FakeMainframe {
            agents_by_name: Default::default(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: Default::default(),
            recorded: Vec::new(),
        };
        let resp = run_dispatch(
            "Think",
            r#"{"note":"file 1 looks like an assignation"}"#,
            &mut mainframe,
            &mut hangar,
            "parent",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert!(collect_text(&resp.content).contains("file 1 looks like an assignation"));
        assert!(collect_text(&resp.content).starts_with("noted:"));
        // Crucially: no hangar or mainframe calls were made — this is
        // a purely in-process tool.
        assert!(hangar.recorded.is_empty());
    }

    #[tokio::test]
    async fn dispatch_think_invalid_json_returns_is_error() {
        let mut mainframe = FakeMainframe {
            agents_by_name: Default::default(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: Default::default(),
            recorded: Vec::new(),
        };
        let resp = run_dispatch("Think", "{not json}", &mut mainframe, &mut hangar, "parent")
            .await
            .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("invalid arguments"));
    }

    #[tokio::test]
    async fn dispatch_agent_calls_mainframe_and_hangar_then_returns_text() {
        let mut mainframe = FakeMainframe {
            agents_by_name: [("alice".to_string(), "alice persona".to_string())]
                .into_iter()
                .collect(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: vec![end_turn("alice says hello")].into(),
            recorded: Vec::new(),
        };
        let resp = run_dispatch(
            "Agent",
            r#"{"name":"alice","query":"hi"}"#,
            &mut mainframe,
            &mut hangar,
            "parent-conv",
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        assert_eq!(collect_text(&resp.content), "alice says hello");
        assert_eq!(hangar.recorded.len(), 1);
        let sent = &hangar.recorded[0];
        assert_eq!(sent.system.as_deref(), Some("alice persona"));
        // Sub-conversation id is minted locally (a fresh uuid), distinct
        // from the parent, and carried as the correlation id.
        assert!(!sent.conversation_id.is_empty());
        assert_ne!(sent.conversation_id, "parent-conv");
        assert_eq!(sent.correlation_id.as_deref(), Some("parent-conv"));
        assert_eq!(sent.role, Some(TurnRole::Delegate as i32));
    }

    #[tokio::test]
    async fn dispatch_agent_missing_persona_returns_is_error() {
        let mut mainframe = FakeMainframe {
            agents_by_name: Default::default(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: Default::default(),
            recorded: Vec::new(),
        };
        let resp = run_dispatch(
            "Agent",
            r#"{"name":"ghost","query":"hi"}"#,
            &mut mainframe,
            &mut hangar,
            "parent",
        )
        .await
        .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("ghost"));
    }

    #[tokio::test]
    async fn dispatch_agent_subagent_tool_use_stop_returns_is_error() {
        // Sub-agents run a single round-trip with no tools. A sub-agent that
        // stops on ToolUse (tried to call a tool) must surface as an
        // LLM-visible error so the orchestrator can rephrase — not a silent
        // success. Mutant: fold ToolUse into the EndTurn|MaxTokens success arm
        // → is_error becomes false, red.
        let mut mainframe = FakeMainframe {
            agents_by_name: [("helper".to_string(), "helper persona".to_string())]
                .into_iter()
                .collect(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: vec![tool_use("partial")].into(),
            recorded: Vec::new(),
        };
        let resp = run_dispatch(
            "Agent",
            r#"{"name":"helper","query":"hi"}"#,
            &mut mainframe,
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
        // Empty `name` is mainframe's "primary AGENTS.md" convention;
        // letting it through here would silently self-dispatch the
        // orchestrator. Must reject without calling mainframe.
        let mut mainframe = FakeMainframe {
            agents_by_name: Default::default(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: Default::default(),
            recorded: Vec::new(),
        };
        let resp = run_dispatch(
            "Agent",
            r#"{"name":"","query":"hi"}"#,
            &mut mainframe,
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
        let mut mainframe = FakeMainframe {
            agents_by_name: Default::default(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: Default::default(),
            recorded: Vec::new(),
        };
        let resp = run_dispatch("Agent", "{not json}", &mut mainframe, &mut hangar, "parent")
            .await
            .unwrap();
        assert!(resp.is_error);
        assert!(collect_text(&resp.content).contains("invalid Agent arguments"));
    }

    #[tokio::test]
    async fn dispatch_agents_returns_projected_json() {
        let mut mainframe = FakeMainframe {
            agents_by_name: Default::default(),
            listed: vec![
                AgentInfo {
                    name: "alice".into(),
                    description: "legal".into(),
                },
                AgentInfo {
                    name: "bob".into(),
                    description: "ops".into(),
                },
            ],
        };
        let mut hangar = FakeHangar {
            turns: Default::default(),
            recorded: Vec::new(),
        };
        let resp = run_dispatch("Agents", "{}", &mut mainframe, &mut hangar, "parent")
            .await
            .unwrap();
        assert!(!resp.is_error);
        let parsed: Vec<AgentInfoJson> =
            serde_json::from_str(&collect_text(&resp.content)).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "alice");
        assert_eq!(parsed[0].description, "legal");
    }

    #[tokio::test]
    async fn recent_turns_reads_log_tail_without_mutating() {
        use hangar_providers::types::{ContentBlock, Message};
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

        let mut mainframe = FakeMainframe {
            agents_by_name: Default::default(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: Default::default(),
            recorded: Vec::new(),
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let resp = dispatch(
            "RecentTurns",
            r#"{"limit":5}"#,
            &mut mainframe,
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
        // Read-only: the log is unchanged and no turn was dispatched.
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
        let mut mainframe = FakeMainframe {
            agents_by_name: Default::default(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: Default::default(),
            recorded: Vec::new(),
        };
        let err = run_dispatch("Ghost", "{}", &mut mainframe, &mut hangar, "parent")
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

    // Subagent dispatch must not be opaque: the transponder must DELIVER the
    // sub-agent's streamed frames to the gateway instead of dropping them
    // through `NullSink`. This pins that delivery independently: a dispatched
    // sub-agent turn whose hangar source yields a ContentDelta must produce at
    // least one `RelayRpc::deliver_stream_item` call on the wire, carrying
    // the parent<->child correlation link.

    use hangar_proto::{turn_event as te, ContentDelta};
    use proto_common::StreamItem;

    /// A streamed assistant-text event — the sub-agent turn must emit a frame
    /// for this, and that frame must reach the gateway. The all-`Complete`
    /// scripts used by the other tests never open a stream item, so they can't
    /// exercise delivery.
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

    /// Records every `deliver_stream_item` call. Models the `Capturing` sink in
    /// `turn.rs`, but at the RPC boundary the sub-agent path must reach — this
    /// is the wire the frames either cross or (today) never do.
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
        // Materiality: the drop site is the `NullSink` in `dispatch_agent`
        // (runtime_tools.rs ~:302). The production change that makes this pass
        // is replacing that `NullSink` with a live `GatewaySink { rpc,
        // channel_id }` bound to the turn's reply channel. Flip the sink back
        // to `NullSink` and zero `deliver_stream_item` calls are recorded ->
        // this reds again on behavior (frames dropped), not on a symbol.
        //
        // Distinct from turn.rs `subagent_frames_carry_parent_conversation_id`
        // (which pins the STAMP on a frame in isolation): this pins that the
        // stamped frame actually crosses the transponder->gateway wire.
        let mut mainframe = FakeMainframe {
            agents_by_name: [("scout".to_string(), "scout persona".to_string())]
                .into_iter()
                .collect(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: vec![content_delta_then_end("looking...", "done")].into(),
            recorded: Vec::new(),
        };
        let mut relay = CapturingRelay { delivered: vec![] };
        let registry = test_registry();
        let cancel = tokio_util::sync::CancellationToken::new();

        let resp = dispatch_agent(
            r#"{"name":"scout","query":"find it"}"#,
            &mut mainframe,
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

        // The load-bearing check: the sub-agent's streamed frame reached the
        // gateway. With today's NullSink, `delivered` is empty and this fails.
        assert!(
            !relay.delivered.is_empty(),
            "sub-agent streamed frames must be delivered to the gateway, not dropped"
        );

        // Corroboration: a delivered frame carries the parent<->child link the
        // client groups by (parent id in `parent_conversation_id`, child id in
        // `conversation_id`) and targets the turn's reply channel.
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
        // The delivered frame carries the dispatched agent's name end-to-end.
        // Materiality: `dispatch_agent` wires the sub-request's agent name into
        // the frame stamp at runtime_tools.rs:321 (`args.name.clone()` passed to
        // `EmitState::new_subagent`). Mutate that arg to `String::new()` and the
        // delivered frame's `agent_name` goes empty -> this reds. The turn.rs
        // stamp tests can't catch that mutant: they build `new_subagent(...,
        // "poet")` directly, bypassing this wire.
        assert_eq!(
            item.agent_name, "scout",
            "the delivered frame must carry the dispatched agent's name"
        );
    }

    // These pin the dispatch-path half of the cascade: the sub-agent stream
    // consumer must observe the parent turn's cancellation signal and abandon,
    // an uncancelled sub-agent must still run to completion unchanged, and the
    // child sub-conversation must NOT be registered as a second cancellable turn.

    use tokio_util::sync::CancellationToken;

    // When the parent cancel fires while a sub-agent's model stream is still
    // yielding events, the consumer stops reading and returns a cancelled
    // outcome rather than draining the stream to its natural end.
    #[tokio::test]
    async fn dispatch_agent_cancels_subagent_stream_instead_of_draining() {
        // A pre-fired token + an endless sub-agent stream. The sub-agent
        // consumer must observe the cancel at the next event boundary and
        // return `DispatchAbort::Cancelled` WITHOUT draining the endless
        // source.
        //
        // Materiality: today `dispatch_agent` calls the non-cancellable
        // `consume_turn_stream` (runtime_tools.rs:324), which fabricates a
        // fresh never-fired token internally — the passed cancel is ignored and
        // the endless source drains forever (this test hangs/times out). The
        // production change that makes it pass is swapping to
        // `consume_turn_stream_cancellable(..., cancel)`. Revert that swap and
        // the endless stream is never abandoned -> red on behavior, not symbol.
        let mut mainframe = FakeMainframe {
            agents_by_name: [("scout".to_string(), "scout persona".to_string())]
                .into_iter()
                .collect(),
            listed: vec![],
        };
        let mut hangar = EndlessHangar;
        let registry = test_registry();
        let cancel = CancellationToken::new();
        cancel.cancel(); // fired before the first poll

        let outcome = dispatch_agent(
            r#"{"name":"scout","query":"go"}"#,
            &mut mainframe,
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

    // While a sub-agent is executing and no cancellation has fired, it runs to
    // normal completion and returns its result unchanged.
    #[tokio::test]
    async fn dispatch_agent_uncancelled_runs_to_normal_completion() {
        // An un-fired token: the cancel arm must never trip; the sub-agent runs
        // its single round-trip and returns the assistant text as a success
        // tool result — identical to the pre-cancellation behavior.
        //
        // Materiality: this is the guard against an over-eager cancel check
        // (e.g. treating a live-but-un-fired token as cancelled, or biasing the
        // select toward cancel unconditionally). Wire the consumer to return
        // Cancelled regardless of token state and this reds: output no longer
        // equals "scout says hi" and is_error/abort diverge from success.
        let mut mainframe = FakeMainframe {
            agents_by_name: [("scout".to_string(), "scout persona".to_string())]
                .into_iter()
                .collect(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: vec![end_turn("scout says hi")].into(),
            recorded: Vec::new(),
        };
        let registry = test_registry();
        let cancel = CancellationToken::new(); // never fired

        let resp = dispatch_agent(
            r#"{"name":"scout","query":"hi"}"#,
            &mut mainframe,
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

    // A tool dispatched while the turn holds its cancellation signal receives
    // that same signal (or a clone), so firing the turn's signal is observable
    // by the dispatched work. This drives the fan-out entry point (`dispatch`)
    // — not `dispatch_agent` directly — with a fired token routed to the `Agent`
    // arm, proving the token survives the `dispatch` -> `dispatch_agent` hop.
    #[tokio::test]
    async fn dispatch_forwards_cancel_to_the_agent_arm() {
        // Distinct from the consumer-swap mutant inside dispatch_agent: THIS
        // test's mutant lives one layer up in `dispatch` —
        // dropping the received `cancel` and handing `dispatch_agent` a fresh
        // `&CancellationToken::new()` instead of forwarding the fired one. Under
        // that mutant the endless sub-agent drains forever (hang/timeout)
        // instead of surfacing `DispatchAbort::Cancelled` -> red on behavior.
        let mut mainframe = FakeMainframe {
            agents_by_name: [("scout".to_string(), "scout persona".to_string())]
                .into_iter()
                .collect(),
            listed: vec![],
        };
        let mut hangar = EndlessHangar;
        let registry = test_registry();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = dispatch(
            "Agent",
            r#"{"name":"scout","query":"go"}"#,
            &mut mainframe,
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

    // When a sub-agent is dispatched, the conversation registry must not gain a
    // second registered turn or signal for it; the sub-agent is cancellable
    // solely by the parent turn's signal.
    #[tokio::test]
    async fn dispatch_agent_does_not_register_a_second_turn_for_the_child() {
        // The child sub-conversation id is minted (mint()) but must never be
        // register_turn()'d — registering it would detach the child from the
        // parent's token and silently defeat the cascade. We prove no turn is
        // registered for the child by asking the registry to cancel it: with no
        // registered turn, cancel() reports false.
        //
        // Materiality: add a `registry.register_turn(&child_id)` in
        // `dispatch_agent` (the exact mistake the constraint forbids) and
        // `cancel(child_id)` would return true -> this reds. The uncancelled
        // token here keeps the sub-agent on the normal-completion path so the
        // only thing under test is the registration side effect.
        let mut mainframe = FakeMainframe {
            agents_by_name: [("scout".to_string(), "scout persona".to_string())]
                .into_iter()
                .collect(),
            listed: vec![],
        };
        let mut hangar = FakeHangar {
            turns: vec![end_turn("done")].into(),
            recorded: Vec::new(),
        };
        let registry = test_registry();
        let cancel = CancellationToken::new();

        let _ = dispatch_agent(
            r#"{"name":"scout","query":"hi"}"#,
            &mut mainframe,
            &mut hangar,
            &registry,
            "parent-conv",
            None,
            None,
            &cancel,
        )
        .await
        .expect("sub-agent completes normally");

        // The child id is the one the sub-request was minted with.
        let child_id = hangar.recorded[0].conversation_id.clone();
        assert_ne!(child_id, "parent-conv");
        assert!(
            !registry.cancel(&child_id).await,
            "the child sub-conversation must NOT be a registered (independently cancellable) turn"
        );
    }
}
