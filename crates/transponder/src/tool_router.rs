//! Tool router: fan-in across mainframe-ctrl, airlock-ctrl, and the
//! transponder-local runtime (Agent / Agents).
//!
//! Every tool the LLM sees has a `Source`. `Mainframe` and `Airlock` tools
//! advertise themselves via gRPC streams from their controllers and dispatch
//! via gRPC unary calls. `Runtime` tools (`Agent`, `Agents`) are statically
//! defined here and dispatched in-process — they compose authoritative
//! controller calls (mainframe `GetAgent` / `ListAgents` + tightbeam `Turn`)
//! and never fabricate results.

use std::sync::Arc;

use airlock_proto::{CallToolResponse, ToolInfo};
use arc_swap::ArcSwap;
use tightbeam_proto::ToolDefinition;
use tokio_stream::StreamExt;

use crate::channel_tools;
use crate::clients::{AirlockClient, MainframeClient, TightbeamRpc};
use crate::runtime_tools;

/// Which subsystem owns a given tool name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Mainframe,
    Airlock,
    Runtime,
    /// Client-side tool. Executes on the user's device (Flutter app
    /// today) via a `ServerRequest` over the channel. Dispatch routes
    /// through `tightbeam.send_server_notification` or
    /// `send_server_request_and_await` depending on the tool's `Kind`.
    Channel,
}

/// Tool dispatch surface used by the agent loop. The trait carries the
/// runtime context (tightbeam, conversation id) so `Runtime`-source
/// tools can compose controller calls without the router having to own
/// its own tightbeam handle. `tool_definitions` lives on the concrete
/// `ToolRouter` instead — the loop reads it directly via the snapshot.
#[async_trait::async_trait]
pub(crate) trait ToolDispatcher: Send + Sync {
    async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        tightbeam: &mut dyn TightbeamRpc,
        conversation_id: &str,
        reply_channel: Option<&str>,
        tool_call_id: &str,
    ) -> Result<CallToolResponse, String>;
}

pub(crate) struct ToolRouter {
    mainframe: Option<MainframeClient>,
    airlock: Option<AirlockClient>,
    /// Live snapshot keyed by tool name. Mainframe and airlock pushes
    /// overwrite their own entries; runtime tools are inserted at
    /// construction time and never change. Lock-free reads via
    /// `ArcSwap`; writers serialize through `apply_lock`.
    tools: ArcSwap<Vec<(ToolInfo, Source)>>,
    /// Serializes the two `apply_*_tools` watcher tasks so concurrent
    /// read-modify-swap can't drop one source's update. No `.await`
    /// crosses the guard, so `std::sync::Mutex` is correct.
    apply_lock: std::sync::Mutex<()>,
}

impl ToolRouter {
    pub(crate) fn new(mainframe: Option<MainframeClient>, airlock: Option<AirlockClient>) -> Self {
        let mut tools: Vec<(ToolInfo, Source)> = runtime_tools::tool_definitions()
            .into_iter()
            .map(|t| (t, Source::Runtime))
            .collect();
        // Channel-source tools (RevealPath, RequestUserInput, RequestUserAuth)
        // are framework-defined just like runtime tools — declared in
        // `channel_tools::tool_definitions` and inserted at construction.
        tools.extend(
            channel_tools::tool_definitions()
                .into_iter()
                .map(|t| (t, Source::Channel)),
        );
        // Ensure runtime + channel names don't collide among themselves.
        let mut seen = std::collections::HashSet::new();
        for (info, _) in &tools {
            assert!(
                seen.insert(info.name.clone()),
                "duplicate framework tool: {}",
                info.name
            );
        }
        // Sort for deterministic advertisement order.
        tools.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        Self {
            mainframe,
            airlock,
            tools: ArcSwap::new(Arc::new(tools)),
            apply_lock: std::sync::Mutex::new(()),
        }
    }

    /// Replace the mainframe-owned subset of the tool list with a fresh
    /// snapshot. Runtime + airlock entries are preserved. Errors hard on
    /// any name collision with an existing source.
    pub(crate) fn apply_mainframe_tools(&self, tools: Vec<ToolInfo>) -> Result<(), String> {
        self.apply_source(Source::Mainframe, tools)
    }

    /// Replace the airlock-owned subset of the tool list with a fresh
    /// snapshot. Runtime + mainframe entries are preserved. Errors hard on
    /// any name collision with an existing source.
    pub(crate) fn apply_airlock_tools(&self, tools: Vec<ToolInfo>) -> Result<(), String> {
        self.apply_source(Source::Airlock, tools)
    }

    fn apply_source(&self, source: Source, snapshot: Vec<ToolInfo>) -> Result<(), String> {
        // Serialize concurrent watcher RMWs. The collision check below
        // reads the live snapshot — both halves must run inside the
        // writer-lock scope or two simultaneous applies of colliding
        // names could both pass it.
        let _guard = self
            .apply_lock
            .lock()
            .expect("apply_lock poisoned — unrecoverable");
        let current = self.tools.load();
        // Detect collisions against entries owned by a different source.
        // Runtime ones are framework-defined; mainframe/airlock are
        // operator-configured. Either side colliding with another is a
        // configuration bug we want to surface loudly.
        for tool in &snapshot {
            for (existing_tool, existing_source) in current.iter() {
                if existing_tool.name == tool.name && *existing_source != source {
                    return Err(format!(
                        "tool name collision: {} advertised by both {:?} and {:?}",
                        tool.name, existing_source, source
                    ));
                }
            }
        }
        let mut next: Vec<(ToolInfo, Source)> = current.iter().cloned().collect();
        next.retain(|(_, s)| *s != source);
        next.extend(snapshot.into_iter().map(|t| (t, source)));
        next.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        let len = next.len();
        self.tools.store(Arc::new(next));
        tracing::info!(count = len, source = ?source, "tool router refreshed");
        Ok(())
    }

    pub(crate) fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .load()
            .iter()
            .map(|(t, _)| ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters_json: t.parameters_json.clone(),
            })
            .collect()
    }

    fn source_of(&self, name: &str) -> Option<Source> {
        self.tools
            .load()
            .iter()
            .find(|(t, _)| t.name == name)
            .map(|(_, s)| *s)
    }

    pub(crate) async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        tightbeam: &mut dyn TightbeamRpc,
        conversation_id: &str,
        reply_channel: Option<&str>,
        tool_call_id: &str,
    ) -> Result<CallToolResponse, String> {
        let source = self
            .source_of(name)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        match source {
            Source::Airlock => {
                let mut client = self
                    .airlock
                    .clone()
                    .ok_or("airlock client not configured")?;
                client.call_tool(name, input_json).await
            }
            Source::Mainframe => {
                let mut client = self
                    .mainframe
                    .clone()
                    .ok_or("mainframe client not configured")?;
                let resp = client.call_tool(name, input_json).await?;
                // mainframe-proto and airlock-proto declare structurally
                // identical CallToolResponse types but they're distinct
                // Rust types. Map to the airlock shape so the agent loop
                // doesn't have to know which controller served the call.
                Ok(CallToolResponse {
                    output: resp.output,
                    is_error: resp.is_error,
                })
            }
            Source::Runtime => {
                let mut mainframe = self
                    .mainframe
                    .clone()
                    .ok_or("mainframe client not configured for runtime tools")?;
                runtime_tools::dispatch(
                    name,
                    input_json,
                    &mut mainframe,
                    tightbeam,
                    conversation_id,
                )
                .await
            }
            Source::Channel => {
                channel_tools::dispatch(name, input_json, tightbeam, reply_channel, tool_call_id)
                    .await
            }
        }
    }
}

#[async_trait::async_trait]
impl ToolDispatcher for ToolRouter {
    async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        tightbeam: &mut dyn TightbeamRpc,
        conversation_id: &str,
        reply_channel: Option<&str>,
        tool_call_id: &str,
    ) -> Result<CallToolResponse, String> {
        ToolRouter::call_tool(
            self,
            name,
            input_json,
            tightbeam,
            conversation_id,
            reply_channel,
            tool_call_id,
        )
        .await
    }
}

/// Background task: hold a `WatchTools` stream open against airlock-ctrl,
/// applying every pushed snapshot to the shared router. Reconnects with
/// backoff on stream error so transient network failures or controller
/// restarts don't permanently detach a workspace from chamber-tool
/// updates.
pub(crate) async fn watch_airlock_tools(
    mut client: AirlockClient,
    router: Arc<ToolRouter>,
    initial_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    let mut initial_tx = initial_tx;
    loop {
        match client.watch_tools().await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(update) => {
                            if let Err(e) = router.apply_airlock_tools(update.tools) {
                                tracing::error!(error = %e, "airlock tool snapshot rejected");
                            }
                            if let Some(tx) = initial_tx.take() {
                                let _ = tx.send(());
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "airlock watch_tools stream error, reconnecting");
                            break;
                        }
                    }
                }
                tracing::info!("airlock watch_tools stream closed, reconnecting");
            }
            Err(e) => {
                tracing::warn!(error = %e, "airlock watch_tools subscribe failed, retrying");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Background task: hold a `WatchTools` stream open against mainframe-ctrl.
/// Mainframe's tool list is static today (Skill + Skills) so the stream
/// emits one snapshot and idles; the reconnect loop is in place for when
/// dynamic refresh lands.
pub(crate) async fn watch_mainframe_tools(
    mut client: MainframeClient,
    router: Arc<ToolRouter>,
    initial_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    let mut initial_tx = initial_tx;
    loop {
        match client.watch_tools().await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(update) => {
                            // mainframe-proto's ToolInfo is structurally
                            // identical to airlock-proto's but a distinct
                            // Rust type — convert on the way in.
                            let converted = update
                                .tools
                                .into_iter()
                                .map(|t| ToolInfo {
                                    name: t.name,
                                    description: t.description,
                                    parameters_json: t.parameters_json,
                                })
                                .collect();
                            if let Err(e) = router.apply_mainframe_tools(converted) {
                                tracing::error!(error = %e, "mainframe tool snapshot rejected");
                            }
                            if let Some(tx) = initial_tx.take() {
                                let _ = tx.send(());
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "mainframe watch_tools stream error, reconnecting");
                            break;
                        }
                    }
                }
                tracing::info!("mainframe watch_tools stream closed, reconnecting");
            }
            Err(e) => {
                tracing::warn!(error = %e, "mainframe watch_tools subscribe failed, retrying");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::TurnSource;
    use tightbeam_proto::TurnRequest;

    struct FakeTightbeam;

    #[async_trait::async_trait]
    impl TightbeamRpc for FakeTightbeam {
        async fn turn(&mut self, _request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
            Err("FakeTightbeam::turn not used by these tests".into())
        }
        async fn mint_conversation(&mut self) -> Result<String, String> {
            Err("FakeTightbeam::mint not used by these tests".into())
        }
        async fn send_server_notification(
            &mut self,
            _channel_id: &str,
            _method: &str,
            _params_json: &str,
        ) -> Result<bool, String> {
            Err("FakeTightbeam::send_server_notification not used by these tests".into())
        }
        async fn send_server_request_and_await(
            &mut self,
            _channel_id: &str,
            _request_id: &str,
            _method: &str,
            _params_json: &str,
            _timeout_seconds: u32,
        ) -> Result<crate::clients::ServerRequestOutcome, String> {
            Err("FakeTightbeam::send_server_request_and_await not used by these tests".into())
        }
    }

    fn t(name: &str) -> ToolInfo {
        ToolInfo {
            name: name.into(),
            description: format!("desc:{name}"),
            parameters_json: "{}".into(),
        }
    }

    fn empty_router() -> ToolRouter {
        ToolRouter::new(None, None)
    }

    fn names(router: &ToolRouter) -> Vec<String> {
        router
            .tool_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    #[test]
    fn new_router_advertises_runtime_tools() {
        let router = empty_router();
        let names = names(&router);
        assert!(names.iter().any(|n| n == "Agent"));
        assert!(names.iter().any(|n| n == "Agents"));
    }

    #[test]
    fn apply_airlock_tools_preserves_runtime_entries() {
        let router = empty_router();
        router
            .apply_airlock_tools(vec![t("Bash"), t("Git")])
            .unwrap();
        let names = names(&router);
        assert!(names.iter().any(|n| n == "Agent"));
        assert!(names.iter().any(|n| n == "Bash"));
        assert!(names.iter().any(|n| n == "Git"));
    }

    #[test]
    fn apply_mainframe_tools_preserves_airlock_entries() {
        let router = empty_router();
        router.apply_airlock_tools(vec![t("Bash")]).unwrap();
        router.apply_mainframe_tools(vec![t("Skill")]).unwrap();
        let names = names(&router);
        assert!(names.iter().any(|n| n == "Bash"));
        assert!(names.iter().any(|n| n == "Skill"));
        assert!(names.iter().any(|n| n == "Agent"));
    }

    #[test]
    fn apply_replaces_within_same_source() {
        let router = empty_router();
        router.apply_airlock_tools(vec![t("Bash")]).unwrap();
        router.apply_airlock_tools(vec![t("Git")]).unwrap();
        let names = names(&router);
        assert!(!names.iter().any(|n| n == "Bash"));
        assert!(names.iter().any(|n| n == "Git"));
    }

    #[test]
    fn apply_rejects_cross_source_collision() {
        let router = empty_router();
        router.apply_airlock_tools(vec![t("Skill")]).unwrap();
        let err = router.apply_mainframe_tools(vec![t("Skill")]).unwrap_err();
        assert!(err.contains("collision"));
    }

    #[test]
    fn apply_rejects_collision_with_runtime_tool() {
        let router = empty_router();
        let err = router.apply_airlock_tools(vec![t("Agent")]).unwrap_err();
        assert!(err.contains("collision"));
    }

    #[tokio::test]
    async fn call_tool_unknown_name_rejected() {
        let router = empty_router();
        let mut tb = FakeTightbeam;
        let err = router
            .call_tool("Nope", "{}", &mut tb, "conv", None, "tc")
            .await
            .unwrap_err();
        assert!(err.contains("unknown tool"));
    }

    #[tokio::test]
    async fn call_tool_routes_airlock_through_airlock_client() {
        let router = empty_router();
        router.apply_airlock_tools(vec![t("Bash")]).unwrap();
        let mut tb = FakeTightbeam;
        let err = router
            .call_tool("Bash", "{}", &mut tb, "conv", None, "tc")
            .await
            .unwrap_err();
        // No airlock client wired in this test; the routing decision
        // proves the source attribution worked.
        assert!(err.contains("airlock client not configured"));
    }

    #[tokio::test]
    async fn call_tool_routes_mainframe_through_mainframe_client() {
        let router = empty_router();
        router.apply_mainframe_tools(vec![t("Skill")]).unwrap();
        let mut tb = FakeTightbeam;
        let err = router
            .call_tool("Skill", "{}", &mut tb, "conv", None, "tc")
            .await
            .unwrap_err();
        assert!(err.contains("mainframe client not configured"));
    }

    #[tokio::test]
    async fn call_tool_routes_runtime_through_runtime_dispatch() {
        let router = empty_router();
        let mut tb = FakeTightbeam;
        // No mainframe client wired; the routing decision proves Runtime
        // source attribution worked (the call would otherwise hit the
        // "unknown tool" branch).
        let err = router
            .call_tool("Agent", "{}", &mut tb, "conv", None, "tc")
            .await
            .unwrap_err();
        assert!(err.contains("mainframe client not configured for runtime tools"));
    }

    #[test]
    fn source_of_returns_correct_attribution() {
        let router = empty_router();
        router.apply_airlock_tools(vec![t("Bash")]).unwrap();
        router.apply_mainframe_tools(vec![t("Skill")]).unwrap();
        assert_eq!(router.source_of("Bash"), Some(Source::Airlock));
        assert_eq!(router.source_of("Skill"), Some(Source::Mainframe));
        assert_eq!(router.source_of("Agent"), Some(Source::Runtime));
        assert_eq!(router.source_of("Ghost"), None);
    }
}
