//! Tool router for tools served by external runtimes (mainframe + airlock).
//!
//! Transponder built-ins (e.g., `llm_call`) are NOT routed here — they're
//! advertised to the LLM at the call site (see `runtime_entrypoint.rs`) but
//! dispatched by the agent loop directly because they need privileged access
//! to transponder's own state (tightbeam client, the router itself for
//! delegate sub-calls, max_iterations).

use std::collections::HashMap;
use std::sync::Arc;

use airlock_proto::{CallToolResponse, ToolInfo};
use tightbeam_proto::ToolDefinition;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;

use crate::clients::{AirlockClient, ToolClient};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

    /// Populate the router with mainframe (stdlib) tools. Airlock tools
    /// arrive separately via the background `WatchTools` subscription's first
    /// snapshot — see `main.rs` for the oneshot that gates message processing
    /// on first apply.
    pub(crate) async fn initialize(&mut self) -> Result<(), String> {
        let mainframe_tools = self.mainframe.list_tools().await?;

        self.tools.clear();
        self.routes.clear();

        for tool in &mainframe_tools {
            self.routes.insert(tool.name.clone(), ToolSource::Mainframe);
        }
        self.tools.extend(mainframe_tools);

        tracing::info!(count = self.tools.len(), "tool router initialized");

        Ok(())
    }

    /// Replace the airlock-sourced subset of the router's tool list with
    /// `airlock_tools`. Mainframe-sourced entries are preserved untouched.
    /// Used by the background `WatchTools` task to apply server-pushed
    /// snapshots without going through `initialize()` (which would also
    /// re-fetch mainframe tools and require an extra RPC).
    ///
    /// On name collisions across the new airlock set vs. existing mainframe
    /// entries, mainframe wins (same precedence as `initialize`).
    pub(crate) fn apply_airlock_tools(&mut self, airlock_tools: Vec<ToolInfo>) {
        // Drop everything currently routed to airlock.
        self.tools
            .retain(|t| self.routes.get(&t.name) != Some(&ToolSource::Airlock));
        self.routes.retain(|_, v| *v != ToolSource::Airlock);

        // Insert new airlock entries, skipping any name already held by mainframe.
        for tool in airlock_tools {
            if matches!(self.routes.get(&tool.name), Some(ToolSource::Mainframe)) {
                tracing::warn!(
                    tool = %tool.name,
                    "airlock tool collides with existing mainframe tool, mainframe wins"
                );
                continue;
            }
            self.routes.insert(tool.name.clone(), ToolSource::Airlock);
            self.tools.push(tool);
        }

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
    use airlock_proto::ToolInfo;

    fn t(name: &str) -> ToolInfo {
        ToolInfo {
            name: name.into(),
            description: format!("desc:{name}"),
            parameters_json: "{}".into(),
        }
    }

    /// Construct a router with pre-populated entries directly (bypassing
    /// `new()` which requires real client connections). Tests only the
    /// in-memory mutation path of `apply_airlock_tools`.
    fn router_with(airlock_names: &[&str], mainframe_names: &[&str]) -> ToolRouter {
        let mut tools = Vec::new();
        let mut routes = HashMap::new();
        for name in airlock_names {
            tools.push(t(name));
            routes.insert((*name).to_string(), ToolSource::Airlock);
        }
        for name in mainframe_names {
            tools.push(t(name));
            routes.insert((*name).to_string(), ToolSource::Mainframe);
        }
        ToolRouter {
            airlock: None,
            mainframe: ToolClient::stub_for_tests(),
            tools,
            routes,
        }
    }

    #[tokio::test]
    async fn apply_airlock_tools_replaces_airlock_keeps_mainframe() {
        let mut router = router_with(&["ssh", "git"], &["bash", "read_file"]);

        router.apply_airlock_tools(vec![t("kubectl"), t("helm"), t("docker")]);

        let names: Vec<&str> = router.tools.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"bash"), "mainframe tool dropped");
        assert!(names.contains(&"read_file"), "mainframe tool dropped");
        assert!(names.contains(&"kubectl"), "new airlock tool not added");
        assert!(names.contains(&"helm"), "new airlock tool not added");
        assert!(names.contains(&"docker"), "new airlock tool not added");
        assert!(!names.contains(&"ssh"), "old airlock tool not removed");
        assert!(!names.contains(&"git"), "old airlock tool not removed");

        assert_eq!(router.routes.get("bash"), Some(&ToolSource::Mainframe));
        assert_eq!(router.routes.get("kubectl"), Some(&ToolSource::Airlock));
        assert!(!router.routes.contains_key("ssh"));
    }

    #[tokio::test]
    async fn apply_airlock_tools_empty_drops_all_airlock_entries() {
        let mut router = router_with(&["ssh"], &["bash"]);
        router.apply_airlock_tools(vec![]);
        let names: Vec<&str> = router.tools.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["bash"]);
        assert_eq!(router.tools.len(), 1);
        assert_eq!(router.routes.len(), 1);
    }

    #[tokio::test]
    async fn apply_airlock_tools_mainframe_wins_on_collision() {
        let mut router = router_with(&[], &["search"]);
        router.apply_airlock_tools(vec![t("search"), t("kubectl")]);
        // The mainframe `search` survives; the airlock `search` is dropped.
        assert_eq!(router.routes.get("search"), Some(&ToolSource::Mainframe));
        assert_eq!(router.routes.get("kubectl"), Some(&ToolSource::Airlock));
        // Only one `search` entry in tools.
        let search_count = router.tools.iter().filter(|x| x.name == "search").count();
        assert_eq!(search_count, 1);
    }
}
