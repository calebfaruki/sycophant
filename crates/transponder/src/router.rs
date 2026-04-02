use std::collections::HashMap;

use tightbeam_proto::{content_block, ContentBlock, Message, TurnRequest};

use crate::clients::TightbeamClient;
use crate::message_source::MessageSource;
use crate::tool_router::ToolRouter;
use crate::{agent, turn};

pub(crate) fn parse_router_response(
    response_text: &str,
    prompts: &HashMap<String, String>,
    current: &str,
) -> (String, Option<String>) {
    let trimmed = response_text.trim();
    if trimmed.is_empty() {
        return (current.to_string(), None);
    }

    let (agent_part, model) = match trimmed.split_once(':') {
        Some((a, m)) => (a.trim().to_lowercase(), Some(m.trim().to_string())),
        None => (trimmed.to_lowercase(), None),
    };

    if agent_part == "router" || !prompts.contains_key(&agent_part) {
        if !agent_part.is_empty() {
            tracing::warn!(
                chosen = %agent_part,
                current = %current,
                "router returned unknown agent, keeping current"
            );
        }
        return (current.to_string(), None);
    }

    (agent_part, model)
}

pub(crate) async fn run_multi_agent(
    max_iterations: u32,
    tightbeam: &mut TightbeamClient,
    tool_router: &mut ToolRouter,
    message_source: &mut dyn MessageSource,
    prompts: &HashMap<String, String>,
) -> Result<(), String> {
    let router_prompt = prompts
        .get("router")
        .ok_or("multi-agent mode requires a 'router' prompt directory")?
        .clone();

    let mut active_agent = prompts
        .keys()
        .find(|k| *k != "router")
        .ok_or("no non-router prompt directories found")?
        .clone();

    let tool_defs = tool_router.tool_definitions();
    let mut first_turn = true;

    loop {
        let content = message_source.next_message().await?;

        let user_msg = Message {
            role: "user".into(),
            content,
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
            agent: None,
        };

        let router_req = TurnRequest {
            system: Some(router_prompt.clone()),
            tools: vec![],
            messages: vec![user_msg],
            agent: Some("router".into()),
            model: None,
        };

        let mut router_stream = tightbeam.turn(router_req).await?;
        let router_result = turn::consume_turn_stream(&mut router_stream).await?;

        let response_text = extract_text(&router_result.content);
        let (chosen_agent, chosen_model) =
            parse_router_response(&response_text, prompts, &active_agent);
        tracing::info!(agent = %chosen_agent, "router selected agent");
        active_agent = chosen_agent;

        let agent_req = TurnRequest {
            system: Some(prompts[&active_agent].clone()),
            tools: if first_turn {
                first_turn = false;
                tool_defs.clone()
            } else {
                vec![]
            },
            messages: vec![],
            agent: Some(active_agent.clone()),
            model: chosen_model,
        };

        agent::tool_loop(
            max_iterations,
            tightbeam,
            tool_router,
            agent_req,
            &active_agent,
        )
        .await?;
    }
}

fn extract_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match &block.block {
            Some(content_block::Block::Text(t)) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_prompts() -> HashMap<String, String> {
        HashMap::from([
            ("research".into(), "prompt".into()),
            ("writer".into(), "prompt".into()),
            ("router".into(), "prompt".into()),
        ])
    }

    #[test]
    fn valid_agent() {
        let prompts = make_prompts();
        let (name, model) = parse_router_response("research", &prompts, "writer");
        assert_eq!(name, "research");
        assert!(model.is_none());
    }

    #[test]
    fn trims_and_lowercases() {
        let prompts = make_prompts();
        let (name, model) = parse_router_response("  Research \n", &prompts, "writer");
        assert_eq!(name, "research");
        assert!(model.is_none());
    }

    #[test]
    fn unknown_keeps_current() {
        let prompts = make_prompts();
        let (name, model) = parse_router_response("nonexistent", &prompts, "research");
        assert_eq!(name, "research");
        assert!(model.is_none());
    }

    #[test]
    fn rejects_router() {
        let prompts = make_prompts();
        let (name, model) = parse_router_response("router", &prompts, "research");
        assert_eq!(name, "research");
        assert!(model.is_none());
    }

    #[test]
    fn empty_keeps_current() {
        let prompts = make_prompts();
        let (name, model) = parse_router_response("", &prompts, "research");
        assert_eq!(name, "research");
        assert!(model.is_none());
    }

    #[test]
    fn agent_with_model_selection() {
        let prompts = make_prompts();
        let (name, model) = parse_router_response("research:claude-opus", &prompts, "writer");
        assert_eq!(name, "research");
        assert_eq!(model.unwrap(), "claude-opus");
    }

    #[test]
    fn agent_with_model_trims() {
        let prompts = make_prompts();
        let (name, model) = parse_router_response("  writer : fast-model  ", &prompts, "research");
        assert_eq!(name, "writer");
        assert_eq!(model.unwrap(), "fast-model");
    }

    #[test]
    fn unknown_agent_with_model_keeps_current() {
        let prompts = make_prompts();
        let (name, model) = parse_router_response("unknown:model", &prompts, "writer");
        assert_eq!(name, "writer");
        assert!(model.is_none());
    }
}
