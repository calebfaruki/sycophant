use pkm_proto::{pkm_event, RunAgentTurn, RunSystemTurn};
use tightbeam_proto::{content_block, ContentBlock, TurnRequest, TurnRole};

use crate::agent;
use crate::clients::{PkmClient, ResolveTurnSession, TightbeamClient};
use crate::message_source::MessageSource;
use crate::tool_router::ToolRouter;
use crate::turn;

pub(crate) async fn run(
    max_iterations: u32,
    tightbeam: &mut TightbeamClient,
    pkm: &mut PkmClient,
    tool_router: &mut ToolRouter,
    message_source: &mut dyn MessageSource,
) -> Result<(), String> {
    let tool_defs = tool_router.tool_definitions();
    let mut first_turn = true;

    loop {
        let inbound = message_source.next_message().await?;

        let mut session = pkm.resolve_turn(inbound.content, inbound.sender).await?;

        let agent_turn = match drive_session(&mut session, tightbeam).await? {
            Some(at) => at,
            None => continue, // ResolveError already delivered or PKM gave up
        };

        let agent_name = agent_turn.agent_name.clone();
        let request = build_agent_turn_request(
            agent_turn,
            &tool_defs,
            &mut first_turn,
            inbound.reply_channel,
        );

        agent::tool_loop(max_iterations, tightbeam, tool_router, request, &agent_name).await?;
    }
}

/// Drive the bidi session until PKM yields a terminal event. Runs system turns
/// against Tightbeam in between.
async fn drive_session(
    session: &mut ResolveTurnSession,
    tightbeam: &mut TightbeamClient,
) -> Result<Option<RunAgentTurn>, String> {
    loop {
        match session.next_event().await? {
            Some(evt) => match evt.event {
                Some(pkm_event::Event::RunSystemTurn(rs)) => {
                    let (response, structured_json) = run_system_turn(rs, tightbeam).await?;
                    session
                        .send_report_system_turn(response, structured_json)
                        .await?;
                }
                Some(pkm_event::Event::RunAgentTurn(ra)) => return Ok(Some(ra)),
                Some(pkm_event::Event::ResolveError(re)) => {
                    tracing::error!(code = re.code, message = %re.message, "pkm resolve error");
                    return Ok(None);
                }
                None => return Err("pkm sent empty event".into()),
            },
            None => return Err("pkm stream closed before terminal event".into()),
        }
    }
}

async fn run_system_turn(
    rs: RunSystemTurn,
    tightbeam: &mut TightbeamClient,
) -> Result<(String, Option<String>), String> {
    let request = TurnRequest {
        system: Some(rs.system_prompt),
        tools: vec![],
        messages: rs.messages,
        agent: Some("system".into()),
        model: None,
        reply_channel: None,
        role: Some(TurnRole::SystemAgent as i32),
        response_schema_json: rs.response_schema_json,
    };
    let mut stream = tightbeam.turn(request).await?;
    let result = turn::consume_turn_stream(&mut stream).await?;
    Ok((extract_text(&result.content), result.structured_json))
}

fn build_agent_turn_request(
    agent_turn: RunAgentTurn,
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
        system: Some(agent_turn.system_prompt),
        tools,
        messages: agent_turn.system_messages,
        agent: Some(agent_turn.agent_name),
        model: None,
        reply_channel,
        role: Some(TurnRole::Agent as i32),
        response_schema_json: None,
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
