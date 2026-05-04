use std::collections::HashMap;

use airlock_proto::{CallToolResponse, ToolInfo};
use tightbeam_proto::ToolDefinition;

use crate::clients::{AirlockClient, ToolClient};
use crate::transponder_tools;

#[derive(Clone, Copy, PartialEq)]
enum ToolSource {
    Airlock,
    Workspace,
    Transponder,
}

pub(crate) struct ToolRouter {
    airlock: Option<AirlockClient>,
    workspace: ToolClient,
    tools: Vec<ToolInfo>,
    routes: HashMap<String, ToolSource>,
}

impl ToolRouter {
    pub(crate) fn new(airlock: Option<AirlockClient>, workspace: ToolClient) -> Self {
        Self {
            airlock,
            workspace,
            tools: Vec::new(),
            routes: HashMap::new(),
        }
    }

    pub(crate) async fn initialize(&mut self) -> Result<(), String> {
        let workspace_tools = self.workspace.list_tools().await?;

        let airlock_tools = match &mut self.airlock {
            Some(client) => client.list_tools().await?,
            None => Vec::new(),
        };

        let transponder_tools = transponder_tools::tool_definitions();

        self.tools.clear();
        self.routes.clear();

        for tool in &airlock_tools {
            self.routes.insert(tool.name.clone(), ToolSource::Airlock);
        }
        self.tools.extend(airlock_tools);

        for tool in &workspace_tools {
            if let Some(existing) = self.routes.insert(tool.name.clone(), ToolSource::Workspace) {
                if existing == ToolSource::Airlock {
                    tracing::warn!(
                        tool = %tool.name,
                        "workspace tool overrides airlock tool with same name"
                    );
                    self.tools.retain(|t| t.name != tool.name);
                }
            }
        }
        self.tools.extend(workspace_tools);

        for tool in &transponder_tools {
            if self
                .routes
                .insert(tool.name.clone(), ToolSource::Transponder)
                .is_some()
            {
                tracing::warn!(
                    tool = %tool.name,
                    "transponder built-in shadows existing tool with same name"
                );
                self.tools.retain(|t| t.name != tool.name);
            }
        }
        self.tools.extend(transponder_tools);

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
            ToolSource::Workspace => self.workspace.call_tool(name, input_json).await,
            ToolSource::Transponder => Err(format!(
                "tool '{name}' is a transponder built-in and must be dispatched by the caller"
            )),
        }
    }
}
