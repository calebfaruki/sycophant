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
use crate::clients::{HangarRpc, MainframeRpc};
use crate::registry::ConversationRegistry;
use crate::turn;

pub(crate) const AGENT_TOOL_NAME: &str = "Agent";
pub(crate) const AGENTS_TOOL_NAME: &str = "Agents";
pub(crate) const THINK_TOOL_NAME: &str = "Think";
pub(crate) const RECENT_TURNS_TOOL_NAME: &str = "RecentTurns";

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
pub(crate) async fn dispatch(
    name: &str,
    input_json: &str,
    mainframe: &mut dyn MainframeRpc,
    hangar: &mut dyn HangarRpc,
    registry: &ConversationRegistry,
    parent_conversation_id: &str,
) -> Result<CallToolResponse, String> {
    match name {
        AGENT_TOOL_NAME => {
            dispatch_agent(
                input_json,
                mainframe,
                hangar,
                registry,
                parent_conversation_id,
            )
            .await
            .or_else(|e| {
                // Tool-call errors flow back to the LLM as is_error tool
                // results; only true infra failures escape via Err.
                // Today every error here is is_error so the orchestrator
                // can see what happened and adjust.
                Ok(CallToolResponse {
                    output: format!("Agent error: {e}"),
                    is_error: true,
                })
            })
        }
        AGENTS_TOOL_NAME => dispatch_agents(mainframe).await.or_else(|e| {
            Ok(CallToolResponse {
                output: format!("Agents error: {e}"),
                is_error: true,
            })
        }),
        THINK_TOOL_NAME => dispatch_think(input_json),
        RECENT_TURNS_TOOL_NAME => {
            dispatch_recent_turns(input_json, registry, parent_conversation_id).await
        }
        other => Err(format!("unknown runtime tool: {other}")),
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
                output: format!("RecentTurns error: invalid arguments: {e}"),
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
        output,
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
            output: format!("noted: {}", args.note),
            is_error: false,
        }),
        Err(e) => Ok(CallToolResponse {
            output: format!("Think error: invalid arguments: {e}"),
            is_error: true,
        }),
    }
}

async fn dispatch_agent(
    input_json: &str,
    mainframe: &mut dyn MainframeRpc,
    hangar: &mut dyn HangarRpc,
    registry: &ConversationRegistry,
    parent_conversation_id: &str,
) -> Result<CallToolResponse, String> {
    let args: AgentArgs =
        serde_json::from_str(input_json).map_err(|e| format!("invalid Agent arguments: {e}"))?;

    // Empty `name` is mainframe's convention for "primary AGENTS.md", used
    // by the orchestrator's per-turn fetch. Allowing it here would silently
    // self-dispatch the orchestrator into a no-tool sub-conversation.
    if args.name.is_empty() {
        return Err(
            "agent name cannot be empty; call Agents() to list available sub-agents".into(),
        );
    }

    let persona = mainframe.get_agent(&args.name).await?;

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
        conversation_id: registry.mint().await?,
    };

    let mut stream = hangar.turn(sub_request).await?;
    // Sub-agent activity is not surfaced to the client in this slice — collapse
    // it as before with a no-op sink (subagent visibility is a later slice).
    let mut sink = turn::NullSink;
    let mut emit = turn::EmitState::new(String::new());
    let scrub = shared::scrub::ScrubSet::from_env_var("__UNSET_SUBAGENT_SCRUB__");
    let outcome = turn::consume_turn_stream(
        &mut *stream,
        turn::DEFAULT_IDLE_GAP,
        &mut sink,
        &mut emit,
        &scrub,
    )
    .await?;

    let text = collect_text(&outcome.content);
    match outcome.stop_reason {
        StopReason::EndTurn | StopReason::MaxTokens => Ok(CallToolResponse {
            output: text,
            is_error: false,
        }),
        StopReason::ToolUse => {
            // Sub-agent has no tools and shouldn't try to call any. If it
            // does, surface as an LLM-visible error so the orchestrator
            // can rephrase the delegation. Hangar already wrote the
            // partial turn to the log.
            Ok(CallToolResponse {
                output: format!(
                    "sub-agent attempted a tool call (no tools available); partial text: {text}"
                ),
                is_error: true,
            })
        }
        other => Ok(CallToolResponse {
            output: format!("sub-agent stopped unexpectedly ({:?}): {text}", other),
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
        output,
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::TurnSource;
    use hangar_proto::{
        content_block, turn_event, ContentBlock, TextBlock, TurnComplete, TurnEvent,
    };
    use mainframe_proto::AgentInfo;
    use std::collections::VecDeque;

    struct FakeMainframe {
        agents_by_name: std::collections::HashMap<String, String>,
        listed: Vec<AgentInfo>,
    }

    #[async_trait::async_trait]
    impl MainframeRpc for FakeMainframe {
        async fn get_agent(&mut self, name: &str) -> Result<String, String> {
            self.agents_by_name
                .get(name)
                .cloned()
                .ok_or_else(|| format!("FakeMainframe: no agent for {name}"))
        }
        async fn list_agents(&mut self) -> Result<Vec<AgentInfo>, String> {
            Ok(self.listed.clone())
        }
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
    ) -> Result<CallToolResponse, String> {
        let registry = test_registry();
        dispatch(name, input, mainframe, hangar, &registry, parent).await
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
        assert!(resp.output.contains("file 1 looks like an assignation"));
        assert!(resp.output.starts_with("noted:"));
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
        assert!(resp.output.contains("invalid arguments"));
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
        assert_eq!(resp.output, "alice says hello");
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
        assert!(resp.output.contains("ghost"));
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
        assert!(resp.output.contains("attempted a tool call"));
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
        assert!(resp.output.contains("name cannot be empty"));
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
        assert!(resp.output.contains("invalid Agent arguments"));
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
        let parsed: Vec<AgentInfoJson> = serde_json::from_str(&resp.output).unwrap();
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
        let resp = dispatch(
            "RecentTurns",
            r#"{"limit":5}"#,
            &mut mainframe,
            &mut hangar,
            &registry,
            &id,
        )
        .await
        .unwrap();
        assert!(!resp.is_error);
        let parsed: Vec<RecentTurnJson> = serde_json::from_str(&resp.output).unwrap();
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
        assert!(err.contains("unknown runtime tool"));
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
}
