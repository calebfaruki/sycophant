//! Tool router for airlock-sourced tools.
//!
//! Per ADR 018, all external tool execution (stdlib and tenant-defined alike)
//! flows through airlock-controller. The router holds an `AirlockClient` and
//! a tool list refreshed by the background `watch_airlock_tools` task.
//!
//! Transponder built-ins (e.g., `llm_call`) are NOT routed here — they're
//! advertised to the LLM at the call site (see `runtime_entrypoint.rs`) but
//! dispatched by the agent loop directly because they need privileged access
//! to transponder's own state (tightbeam client, the router itself for
//! delegate sub-calls, max_iterations).

use std::sync::Arc;

use airlock_proto::{CallToolResponse, ToolInfo};
use tightbeam_proto::ToolDefinition;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;

use crate::clients::AirlockClient;

/// Tool surface the LLM loop consults: which tools exist and how to call them.
/// Production impl is `ToolRouter` (airlock-backed); tests back it with a fake.
#[async_trait::async_trait]
pub(crate) trait ToolDispatcher: Send {
    fn tool_definitions(&self) -> Vec<ToolDefinition>;
    async fn call_tool(&mut self, name: &str, input_json: &str)
        -> Result<CallToolResponse, String>;
}

pub(crate) struct ToolRouter {
    airlock: Option<AirlockClient>,
    tools: Vec<ToolInfo>,
}

impl ToolRouter {
    pub(crate) fn new(airlock: Option<AirlockClient>) -> Self {
        Self {
            airlock,
            tools: Vec::new(),
        }
    }

    /// Replace the router's tool list with the latest snapshot pushed by
    /// airlock-controller. Used by the background `WatchTools` task.
    pub(crate) fn apply_airlock_tools(&mut self, airlock_tools: Vec<ToolInfo>) {
        self.tools = airlock_tools;
        tracing::info!(count = self.tools.len(), "tool router refreshed");
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
        let known = self.tools.iter().any(|t| t.name == name);
        if !known {
            return Err(format!("unknown tool: {name}"));
        }
        let client = self
            .airlock
            .as_mut()
            .ok_or("airlock client not configured")?;
        client.call_tool(name, input_json).await
    }
}

#[async_trait::async_trait]
impl ToolDispatcher for ToolRouter {
    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        ToolRouter::tool_definitions(self)
    }

    async fn call_tool(
        &mut self,
        name: &str,
        input_json: &str,
    ) -> Result<CallToolResponse, String> {
        ToolRouter::call_tool(self, name, input_json).await
    }
}

/// Background task: hold a `WatchTools` stream open, applying every pushed
/// snapshot to the shared router. Reconnects with backoff on stream error so
/// transient network failures or controller restarts don't permanently
/// detach a workspace from chamber-tool updates.
///
/// The first event each subscribe yields is the current snapshot — same
/// content we'd get from a `ListTools` call, so reconnects naturally
/// re-baseline the router.
pub(crate) async fn watch_airlock_tools(
    mut client: AirlockClient,
    router: Arc<Mutex<ToolRouter>>,
    initial_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    let mut initial_tx = initial_tx;
    loop {
        match client.watch_tools().await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(update) => {
                            router.lock().await.apply_airlock_tools(update.tools);
                            if let Some(tx) = initial_tx.take() {
                                let _ = tx.send(());
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "watch_tools stream error, reconnecting");
                            break;
                        }
                    }
                }
                tracing::info!("watch_tools stream closed, reconnecting");
            }
            Err(e) => {
                tracing::warn!(error = %e, "watch_tools subscribe failed, retrying");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str) -> ToolInfo {
        ToolInfo {
            name: name.into(),
            description: format!("desc:{name}"),
            parameters_json: "{}".into(),
        }
    }

    fn router_with(names: &[&str]) -> ToolRouter {
        ToolRouter {
            airlock: None,
            tools: names.iter().map(|n| t(n)).collect(),
        }
    }

    #[test]
    fn apply_airlock_tools_replaces_snapshot() {
        let mut router = router_with(&["ssh", "git"]);
        router.apply_airlock_tools(vec![t("kubectl"), t("helm")]);
        let names: Vec<&str> = router.tools.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["kubectl", "helm"]);
    }

    #[test]
    fn apply_airlock_tools_empty_drops_all() {
        let mut router = router_with(&["Bash", "ssh"]);
        router.apply_airlock_tools(vec![]);
        assert!(router.tools.is_empty());
    }

    #[test]
    fn tool_definitions_mirror_tools_list() {
        let router = router_with(&["Bash", "ReadFile"]);
        let defs = router.tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["Bash", "ReadFile"]);
    }

    #[tokio::test]
    async fn call_tool_unknown_name_rejected() {
        let mut router = router_with(&["Bash"]);
        let err = router.call_tool("git", "{}").await.unwrap_err();
        assert!(err.contains("unknown tool"));
    }

    /// Stage 3 lock: known tools route to the airlock client (single
    /// source). Pre-amendment the router had a `ToolSource::Mainframe`
    /// arm; if it ever returned, this test would see a routing
    /// short-circuit rather than the "airlock client not configured"
    /// signal proving the airlock arm is the only path for known tools.
    #[tokio::test]
    async fn call_tool_known_name_requires_airlock_client() {
        let mut router = router_with(&["Bash"]);
        let err = router.call_tool("Bash", "{}").await.unwrap_err();
        assert!(
            err.contains("airlock client not configured"),
            "known tools must route through the airlock client; got: {err}"
        );
    }
}
