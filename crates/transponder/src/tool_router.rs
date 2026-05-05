//! Tool router for tools served by external runtimes (mainframe + airlock).
//!
//! Transponder built-ins (e.g., `llm_call`) are NOT routed here — they're
//! advertised to the LLM at the call site (see `runtime_entrypoint.rs`) but
//! dispatched by the agent loop directly because they need privileged access
//! to transponder's own state (tightbeam client, the router itself for
//! delegate sub-calls, max_iterations).

use std::collections::HashMap;

use airlock_proto::{CallToolResponse, ToolInfo};
use tightbeam_proto::ToolDefinition;

use crate::clients::{AirlockClient, ToolClient};

#[derive(Clone, Copy, PartialEq)]
enum ToolSource {
    Airlock,
    Mainframe,
}

pub(crate) struct ToolRouter {
    airlock: Option<AirlockClient>,
    mainframe: ToolClient,
    tools: Vec<ToolInfo>,
    routes: HashMap<String, ToolSource>,
}

impl ToolRouter {
    pub(crate) fn new(airlock: Option<AirlockClient>, mainframe: ToolClient) -> Self {
        Self {
            airlock,
            mainframe,
            tools: Vec::new(),
            routes: HashMap::new(),
        }
    }

    pub(crate) async fn initialize(&mut self) -> Result<(), String> {
        let mainframe_tools = self.mainframe.list_tools().await?;

        let airlock_tools = match &mut self.airlock {
            Some(client) => client.list_tools().await?,
            None => Vec::new(),
        };

        self.tools.clear();
        self.routes.clear();

        for tool in &airlock_tools {
            self.routes.insert(tool.name.clone(), ToolSource::Airlock);
        }
        self.tools.extend(airlock_tools);

        for tool in &mainframe_tools {
            if let Some(existing) = self.routes.insert(tool.name.clone(), ToolSource::Mainframe) {
                if existing == ToolSource::Airlock {
                    tracing::warn!(
                        tool = %tool.name,
                        "mainframe tool overrides airlock tool with same name"
                    );
                    self.tools.retain(|t| t.name != tool.name);
                }
            }
        }
        self.tools.extend(mainframe_tools);

        tracing::info!(count = self.tools.len(), "tool router initialized");

        Ok(())
    }

    pub(crate) fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters_json: t.parameters_json.clone(),
            })
            .collect()
    }

    pub(crate) async fn call_tool(
        &mut self,
        name: &str,
        input_json: &str,
    ) -> Result<CallToolResponse, String> {
        let source = self
            .routes
            .get(name)
            .ok_or_else(|| format!("unknown tool: {name}"))?;

        match source {
            ToolSource::Airlock => {
                let client = self
                    .airlock
                    .as_mut()
                    .ok_or("airlock client not configured")?;
                client.call_tool(name, input_json).await
            }
            ToolSource::Mainframe => self.mainframe.call_tool(name, input_json).await,
        }
    }
}
