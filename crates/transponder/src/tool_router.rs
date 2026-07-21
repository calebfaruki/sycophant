//! Tool router: fan-in across mainframe-ctrl, airlock-ctrl, and the
//! transponder-local runtime (Agent / Agents).
//!
//! Every tool the LLM sees has a `Source`. `Mainframe` and `Airlock` tools
//! advertise themselves via gRPC streams from their controllers and dispatch
//! via gRPC unary calls. `Runtime` tools (`Agent`, `Agents`) are statically
//! defined here and dispatched in-process — they compose authoritative
//! controller calls (mainframe `GetAgent` / `ListAgents` + hangar `Turn`)
//! and never fabricate results.

use std::sync::Arc;

use arc_swap::ArcSwap;
use hangar_proto::ToolDefinition;
use proto_common::{CallToolResponse, ToolInfo, ToolListUpdate};
use tokio_stream::StreamExt;

use tokio_util::sync::CancellationToken;

use crate::channel_tools;
use crate::clients::{
    AirlockClient, AirlockRpc, HangarRpc, MainframeClient, MainframeRpc, TightbeamClient,
    TightbeamRpc,
};
use crate::registry::ConversationRegistry;
use crate::runtime_tools::{self, DispatchAbort};

/// Which subsystem owns a given tool name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Mainframe,
    Airlock,
    Runtime,
    /// Client-side tool. Executes on the user's device (Flutter app
    /// today) via a `ServerRequest` over the channel. Dispatch routes
    /// through the tightbeam gateway's `SendServerNotification` /
    /// `SendServerRequestAndAwait` depending on the tool's `Kind`.
    Channel,
}

/// Tool dispatch surface used by the agent loop. The trait carries the
/// runtime context (hangar, conversation id) so `Runtime`-source
/// tools can compose controller calls without the router having to own
/// its own hangar handle. `tool_definitions` lives on the concrete
/// `ToolRouter` instead — the loop reads it directly via the snapshot.
#[async_trait::async_trait]
pub(crate) trait ToolDispatcher: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        hangar: &mut dyn HangarRpc,
        conversation_id: &str,
        reply_channel: Option<&str>,
        tool_call_id: &str,
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort>;
}

pub(crate) struct ToolRouter<M = MainframeClient, A = AirlockClient> {
    /// Generic over the mainframe RPC surface so tests can back the
    /// Mainframe + Runtime arms with a fake (`ToolRouter::<FakeMainframe>::new`)
    /// without a live gRPC server. Production always uses `MainframeClient`,
    /// which the default type parameter selects with no turbofish at call sites.
    mainframe: Option<M>,
    /// Generic over the airlock RPC surface (the second fake seam) so tests
    /// back the `Source::Airlock` arm with a `FakeAirlock`. Production uses
    /// `AirlockClient`, selected by the default type parameter.
    airlock: Option<A>,
    /// Dialer for the tightbeam gateway's internal listener. `Channel`-source
    /// tools push `ServerRequest` frames through it. `None` in tests and when
    /// no gateway is configured.
    tightbeam: Option<TightbeamClient>,
    /// Conversation registry — `Runtime`-source tools reach it for
    /// minting sub-conversations (`Agent`) and reading history
    /// (`RecentTurns`).
    registry: Arc<ConversationRegistry>,
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

impl<M: MainframeRpc + Clone, A: AirlockRpc + Clone + Send + 'static> ToolRouter<M, A> {
    pub(crate) fn new(
        mainframe: Option<M>,
        airlock: Option<A>,
        tightbeam: Option<TightbeamClient>,
        registry: Arc<ConversationRegistry>,
    ) -> Self {
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
            tightbeam,
            registry,
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        hangar: &mut dyn HangarRpc,
        conversation_id: &str,
        reply_channel: Option<&str>,
        tool_call_id: &str,
        // Carried on the trait method so every arm receives it, but only
        // `Source::Runtime` uses it this slice — it cascades the parent turn's
        // cancel into sub-agent dispatch. Load-bearing scaffolding for the
        // chamber slice's `Source::Airlock` consumer; keep it on every arm.
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort> {
        let source = self
            .source_of(name)
            .ok_or_else(|| DispatchAbort::Error(format!("unknown tool: {name}")))?;
        match source {
            Source::Airlock => {
                let mut client = self
                    .airlock
                    .clone()
                    .ok_or_else(|| DispatchAbort::Error("airlock client not configured".into()))?;
                // Learn the call_id before the result exists, then race the
                // parked result wait against the turn's cancel token. Biased so
                // an already-fired token is observed before the result poll.
                let call_id = client
                    .begin_tool_call(name, input_json)
                    .await
                    .map_err(DispatchAbort::Error)?;
                let mut cancel_client = client.clone();
                let cancel_id = call_id.clone();
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        // Fire-and-forget: never await the cancel, so the turn
                        // is not blocked on it. `Cancelled` rides Err up
                        // untouched — never folded into an `Ok(is_error)`.
                        tokio::spawn(async move {
                            let _ = cancel_client.cancel_tool_call(&cancel_id).await;
                        });
                        Err(DispatchAbort::Cancelled)
                    }
                    result = client.await_tool_result(&call_id) => {
                        result.map_err(DispatchAbort::Error)
                    }
                }
            }
            Source::Mainframe => {
                let mut client = self.mainframe.clone().ok_or_else(|| {
                    DispatchAbort::Error("mainframe client not configured".into())
                })?;
                client
                    .call_tool(name, input_json)
                    .await
                    .map_err(DispatchAbort::Error)
            }
            Source::Runtime => {
                let mut mainframe = self.mainframe.clone().ok_or_else(|| {
                    DispatchAbort::Error("mainframe client not configured for runtime tools".into())
                })?;
                let mut gateway = self.tightbeam.clone();
                runtime_tools::dispatch(
                    name,
                    input_json,
                    &mut mainframe,
                    hangar,
                    &self.registry,
                    conversation_id,
                    reply_channel,
                    gateway.as_mut().map(|g| g as &mut dyn TightbeamRpc),
                    cancel,
                )
                .await
            }
            Source::Channel => {
                let mut gateway = self.tightbeam.clone().ok_or_else(|| {
                    DispatchAbort::Error("tightbeam gateway client not configured".into())
                })?;
                channel_tools::dispatch(name, input_json, &mut gateway, reply_channel, tool_call_id)
                    .await
                    .map_err(DispatchAbort::Error)
            }
        }
    }
}

#[async_trait::async_trait]
impl<M: MainframeRpc + Clone + Sync, A: AirlockRpc + Clone + Send + Sync + 'static> ToolDispatcher
    for ToolRouter<M, A>
{
    async fn call_tool(
        &self,
        name: &str,
        input_json: &str,
        hangar: &mut dyn HangarRpc,
        conversation_id: &str,
        reply_channel: Option<&str>,
        tool_call_id: &str,
        cancel: &CancellationToken,
    ) -> Result<CallToolResponse, DispatchAbort> {
        ToolRouter::call_tool(
            self,
            name,
            input_json,
            hangar,
            conversation_id,
            reply_channel,
            tool_call_id,
            cancel,
        )
        .await
    }
}

/// Background task: hold a `WatchTools` stream open against airlock-ctrl,
/// applying every pushed snapshot to the shared router. Reconnects with
/// backoff on stream error so transient network failures or controller
/// restarts don't permanently detach a workspace from chamber-tool
/// updates.
/// Polymorphism seam so one reconnect loop serves both controllers'
/// `WatchTools` streams — both now yield `proto_common::ToolListUpdate`.
#[async_trait::async_trait]
trait ToolCatalogStream: Send {
    async fn watch_tools(&mut self) -> Result<tonic::Streaming<ToolListUpdate>, String>;
}

#[async_trait::async_trait]
impl ToolCatalogStream for AirlockClient {
    async fn watch_tools(&mut self) -> Result<tonic::Streaming<ToolListUpdate>, String> {
        AirlockClient::watch_tools(self).await
    }
}

#[async_trait::async_trait]
impl ToolCatalogStream for MainframeClient {
    async fn watch_tools(&mut self) -> Result<tonic::Streaming<ToolListUpdate>, String> {
        MainframeClient::watch_tools(self).await
    }
}

/// Background task: hold a `WatchTools` stream open against `client` and feed
/// each pushed snapshot to `apply` (the router's per-source setter). Reconnects
/// with backoff so a transient error or controller restart doesn't permanently
/// detach the workspace from tool updates. `component` labels the logs.
async fn watch_tools_loop<C: ToolCatalogStream>(
    mut client: C,
    router: Arc<ToolRouter>,
    apply: fn(&ToolRouter, Vec<ToolInfo>) -> Result<(), String>,
    component: &'static str,
    mut initial_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    loop {
        match client.watch_tools().await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(update) => {
                            if let Err(e) = apply(&router, update.tools) {
                                tracing::error!(error = %e, component, "tool snapshot rejected");
                            }
                            if let Some(tx) = initial_tx.take() {
                                let _ = tx.send(());
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, component, "watch_tools stream error, reconnecting");
                            break;
                        }
                    }
                }
                tracing::info!(component, "watch_tools stream closed, reconnecting");
            }
            Err(e) => {
                tracing::warn!(error = %e, component, "watch_tools subscribe failed, retrying");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

pub(crate) async fn watch_airlock_tools(
    client: AirlockClient,
    router: Arc<ToolRouter>,
    initial_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    watch_tools_loop(
        client,
        router,
        ToolRouter::apply_airlock_tools,
        "airlock",
        initial_tx,
    )
    .await
}

/// Mainframe's tool list is static today (Skill + Skills) so the stream emits
/// one snapshot and idles; the reconnect loop is in place for when dynamic
/// refresh lands.
pub(crate) async fn watch_mainframe_tools(
    client: MainframeClient,
    router: Arc<ToolRouter>,
    initial_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    watch_tools_loop(
        client,
        router,
        ToolRouter::apply_mainframe_tools,
        "mainframe",
        initial_tx,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::TurnSource;
    use hangar_proto::TurnRequest;

    struct FakeHangar;

    #[async_trait::async_trait]
    impl HangarRpc for FakeHangar {
        async fn turn(&mut self, _request: TurnRequest) -> Result<Box<dyn TurnSource>, String> {
            Err("FakeHangar::turn not used by these tests".into())
        }
        async fn cancel_turn(&mut self, _conversation_id: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn t(name: &str) -> ToolInfo {
        ToolInfo {
            name: name.into(),
            description: format!("desc:{name}"),
            parameters_json: "{}".into(),
        }
    }

    fn test_registry() -> Arc<ConversationRegistry> {
        use crate::conversation::{ConversationStoreFactory, LocalFsFactory};
        let root = tempfile::TempDir::new().unwrap().keep();
        let factory: Arc<dyn ConversationStoreFactory> = Arc::new(LocalFsFactory::new(root));
        Arc::new(ConversationRegistry::new(factory))
    }

    fn empty_router() -> ToolRouter {
        ToolRouter::new(None, None, None, test_registry())
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

    /// Assert a dispatch error carries `needle`. The router now returns
    /// `DispatchAbort`; routing-attribution errors surface as `Error(..)`.
    fn assert_dispatch_error(err: DispatchAbort, needle: &str) {
        match err {
            DispatchAbort::Error(e) => assert!(
                e.contains(needle),
                "expected error containing {needle:?}, got {e:?}"
            ),
            DispatchAbort::Cancelled => panic!("expected Error({needle:?}), got Cancelled"),
        }
    }

    #[tokio::test]
    async fn call_tool_unknown_name_rejected() {
        let router = empty_router();
        let mut tb = FakeHangar;
        let cancel = CancellationToken::new();
        let err = router
            .call_tool("Nope", "{}", &mut tb, "conv", None, "tc", &cancel)
            .await
            .unwrap_err();
        assert_dispatch_error(err, "unknown tool");
    }

    #[tokio::test]
    async fn call_tool_routes_airlock_through_airlock_client() {
        let router = empty_router();
        router.apply_airlock_tools(vec![t("Bash")]).unwrap();
        let mut tb = FakeHangar;
        let cancel = CancellationToken::new();
        let err = router
            .call_tool("Bash", "{}", &mut tb, "conv", None, "tc", &cancel)
            .await
            .unwrap_err();
        // No airlock client wired in this test; the routing decision
        // proves the source attribution worked.
        assert_dispatch_error(err, "airlock client not configured");
    }

    #[tokio::test]
    async fn call_tool_routes_mainframe_through_mainframe_client() {
        let router = empty_router();
        router.apply_mainframe_tools(vec![t("Skill")]).unwrap();
        let mut tb = FakeHangar;
        let cancel = CancellationToken::new();
        let err = router
            .call_tool("Skill", "{}", &mut tb, "conv", None, "tc", &cancel)
            .await
            .unwrap_err();
        assert_dispatch_error(err, "mainframe client not configured");
    }

    #[tokio::test]
    async fn call_tool_routes_runtime_through_runtime_dispatch() {
        let router = empty_router();
        let mut tb = FakeHangar;
        let cancel = CancellationToken::new();
        // No mainframe client wired; the routing decision proves Runtime
        // source attribution worked (the call would otherwise hit the
        // "unknown tool" branch).
        let err = router
            .call_tool("Agent", "{}", &mut tb, "conv", None, "tc", &cancel)
            .await
            .unwrap_err();
        assert_dispatch_error(err, "mainframe client not configured for runtime tools");
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

    // ---- ACCEPTANCE (turn-cancel-cascade-subagents, slice 1) ----
    //
    // Spec: ~/vault/projects/sycophant/specs/turn-cancel-cascade-subagents/spec.md
    //
    // AC-2: "Where a tool is dispatched while the turn holds its cancellation
    // signal, the dispatch path shall pass that same signal (or a clone of it)
    // to the dispatched work, such that firing the turn's signal is observable
    // by that work."
    //
    // This pins AC-2 at the ROUTER hop — `ToolRouter::call_tool` forwarding the
    // caller's `cancel` into `runtime_tools::dispatch(...)` in the
    // `Source::Runtime` arm. The sibling test
    // `runtime_tools::dispatch_forwards_cancel_to_the_agent_arm` only proves the
    // lower `dispatch() -> dispatch_agent()` hop; the router's forward of the
    // caller's token had NO coverage (mutating it to a fresh
    // `CancellationToken::new()` left the whole suite green).
    #[tokio::test]
    async fn call_tool_forwards_cancel_into_runtime_dispatch() {
        use crate::test_doubles::{EndlessHangar, FakeMainframe};

        // A router whose Runtime arm reaches `runtime_tools::dispatch`: the
        // generic mainframe seam lets us back it with a fake that returns a
        // persona so `get_agent` succeeds and execution reaches the cancellable
        // sub-agent stream consumer (past the `get_agent` gate).
        let mainframe = FakeMainframe {
            agents_by_name: [("scout".to_string(), "scout persona".to_string())]
                .into_iter()
                .collect(),
            listed: vec![],
        };
        let router: ToolRouter<FakeMainframe> =
            ToolRouter::new(Some(mainframe), None, None, test_registry());

        // The sub-agent's model stream never terminates on its own — only a
        // fired, forwarded cancel can abandon it. If the router dropped the
        // caller's token (M4 at the `runtime_tools::dispatch(...)` forward) and
        // handed dispatch a fresh never-fired token, this drains forever
        // (hang/timeout) instead of returning Cancelled.
        let mut hangar = EndlessHangar;
        let cancel = CancellationToken::new();
        cancel.cancel(); // fired before the first poll

        let outcome = router
            .call_tool(
                "Agent",
                r#"{"name":"scout","query":"go"}"#,
                &mut hangar,
                "parent-conv",
                None,
                "tc",
                &cancel,
            )
            .await;

        // Behavioral assertion (CancellationToken has no PartialEq): observing
        // `DispatchAbort::Cancelled` proves the fired token crossed the router
        // hop and was seen by the dispatched sub-agent work.
        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "call_tool must forward the fired cancel into runtime dispatch, got {outcome:?}"
        );
    }

    // ---- ACCEPTANCE (turn-cancel-cascade-chambers) ----
    //
    // Spec: ~/vault/projects/sycophant/specs/turn-cancel-cascade-chambers/spec.md
    //
    // The `Source::Airlock` arm is the caller in the cascade: it learns the
    // call_id from `begin_tool_call`, then races `await_tool_result` against the
    // turn's cancel token. On cancel it issues exactly one fire-and-forget
    // `cancel_tool_call` and returns the terminal `DispatchAbort::Cancelled`
    // (AC-2 + the chamber half of AC-6); uncancelled, it returns the chamber
    // result unchanged and issues no cancel (AC-8).
    //
    // AC-6's loop half — `Err(DispatchAbort::Cancelled)` driving `llm_loop` to
    // `LoopError::Cancelled` with no tool message appended — is already pinned,
    // source-agnostically, by agent.rs
    // `cancelled_subagent_terminates_loop_without_continuing`; not duplicated here.
    //
    // Expected new surface (does not exist yet — red on the missing arm, not on
    // setup):
    //   trait AirlockRpc { begin_tool_call, await_tool_result, cancel_tool_call }
    //   ToolRouter<M = MainframeClient, A = AirlockClient>  // airlock arm generic
    //   FakeAirlock (crate::test_doubles) backs A in tests

    // AC-2: "When the turn's cancellation signal fires while the caller is
    // awaiting a chamber tool call's result, the caller shall issue exactly one
    // cancel operation for that call's identifier and shall not block the turn
    // awaiting the cancel operation's completion."
    #[tokio::test]
    async fn airlock_cancel_fires_exactly_one_cancel_and_returns_cancelled() {
        use crate::test_doubles::{FakeAirlock, FakeMainframe};

        // `result: None` => await_tool_result pends forever. If the arm awaited
        // the result instead of racing (biased) the already-fired cancel, this
        // test would hang — that hang is the non-blocking clause's teeth.
        let airlock = FakeAirlock::new("call-abc", None);
        let router: ToolRouter<FakeMainframe, FakeAirlock> =
            ToolRouter::new(None, Some(airlock.clone()), None, test_registry());
        router.apply_airlock_tools(vec![t("Bash")]).unwrap();

        let mut hangar = FakeHangar; // the Airlock arm never touches hangar
        let cancel = CancellationToken::new();
        cancel.cancel(); // fired before dispatch

        let outcome = router
            .call_tool("Bash", "{}", &mut hangar, "conv", None, "tc", &cancel)
            .await;

        // Materiality (AC-6 chamber half): folding Cancelled into
        // `Ok(is_error=true)` instead of returning it reds this.
        assert!(
            matches!(outcome, Err(DispatchAbort::Cancelled)),
            "a fired cancel on an Airlock call must return Cancelled, got {outcome:?}"
        );

        // The cancel is fire-and-forget (spawned); poll briefly for it to land.
        let mut recorded = airlock.cancels();
        for _ in 0..200 {
            if !recorded.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            recorded = airlock.cancels();
        }
        // Materiality: firing zero cancels (dropped), two cancels, or a cancel
        // for the wrong id all red this exact-match.
        assert_eq!(
            recorded,
            vec!["call-abc".to_string()],
            "exactly one cancel for the begun call's id must be issued"
        );
    }

    // AC-8: "Where a chamber tool call runs to completion without any
    // cancellation, its result shall be returned unchanged."
    #[tokio::test]
    async fn airlock_uncancelled_returns_result_unchanged() {
        use crate::test_doubles::{FakeAirlock, FakeMainframe};

        let canned = CallToolResponse {
            output: "chamber output".into(),
            is_error: false,
        };
        let airlock = FakeAirlock::new("call-xyz", Some(canned));
        let router: ToolRouter<FakeMainframe, FakeAirlock> =
            ToolRouter::new(None, Some(airlock.clone()), None, test_registry());
        router.apply_airlock_tools(vec![t("Bash")]).unwrap();

        let mut hangar = FakeHangar;
        let cancel = CancellationToken::new(); // never fired

        let resp = router
            .call_tool("Bash", "{}", &mut hangar, "conv", None, "tc", &cancel)
            .await
            .expect("an uncancelled chamber call returns its result");

        // Materiality: altering the result reds the output/is_error asserts; a
        // spurious cancel on the uncancelled path reds the empty-cancels assert.
        assert_eq!(resp.output, "chamber output");
        assert!(!resp.is_error);
        assert!(
            airlock.cancels().is_empty(),
            "no cancel may be issued when the turn was never cancelled"
        );
    }
}
