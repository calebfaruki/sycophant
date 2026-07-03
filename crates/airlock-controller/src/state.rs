use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use shared::scheduling::SchedulingConfig;
use tokio::sync::{oneshot, watch, Mutex, Notify, RwLock};
use tracing::warn;

use crate::crd::Chamber;
use crate::registry::ArgDecl;

#[derive(Clone)]
pub struct WorkspaceBindings {
    map: HashMap<String, Vec<String>>,
}

impl WorkspaceBindings {
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read bindings file {path}: {e}"))?;
        let map: HashMap<String, Vec<String>> = serde_yaml::from_str(&content)
            .map_err(|e| format!("failed to parse bindings YAML: {e}"))?;
        Ok(Self { map })
    }

    pub fn empty() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn from_map(map: HashMap<String, Vec<String>>) -> Self {
        Self { map }
    }

    pub fn chambers_for(&self, workspace: &str) -> &[String] {
        self.map.get(workspace).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn has_chamber(&self, workspace: &str, chamber: &str) -> bool {
        self.chambers_for(workspace).iter().any(|c| c == chamber)
    }
}

impl Default for WorkspaceBindings {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Clone)]
pub struct RegisteredTool {
    pub name: String,
    pub chamber_name: String,
    pub description: String,
    pub image: String,
    pub args: Vec<ArgDecl>,
}

pub struct ToolCallResult {
    pub output: String,
    pub is_error: bool,
    pub exit_code: i32,
}

/// RAII wrapper around a pending tool call's result sender. Guarantees
/// `call_tool` (parked on `result_rx`) always unblocks: on `Drop` without
/// a prior `send`, it emits an error `ToolCallResult`, so a chamber Job
/// reaped before it returned a result can't leave the caller awaiting
/// forever. `oneshot::Sender::send` consumes the sender, so it's held in
/// an `Option` and `take`n on both the success (`send`) and `Drop` paths.
pub struct ToolResultGuard {
    tx: Option<oneshot::Sender<ToolCallResult>>,
}

impl ToolResultGuard {
    pub fn new(tx: oneshot::Sender<ToolCallResult>) -> Self {
        Self { tx: Some(tx) }
    }

    /// Deliver the result and mark complete so `Drop` is a no-op. Returns
    /// the result back if the receiver already went away.
    pub fn send(mut self, result: ToolCallResult) -> Result<(), ToolCallResult> {
        match self.tx.take() {
            Some(tx) => tx.send(result),
            None => Err(result),
        }
    }
}

impl Drop for ToolResultGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(ToolCallResult {
                output: "tool call terminated without a result (chamber reaped or vanished)"
                    .to_string(),
                is_error: true,
                exit_code: -1,
            });
        }
    }
}

pub struct PendingCall {
    pub call_id: String,
    pub tool_name: String,
    pub args: HashMap<String, String>,
    pub working_dir: String,
}

#[derive(Clone)]
pub struct ActiveJob {
    pub job_name: String,
    pub tool_name: String,
    pub last_activity: Instant,
    pub keepalive_seconds: u64,
}

pub struct ControllerState {
    tools: RwLock<HashMap<String, RegisteredTool>>,
    /// Monotonic counter bumped on every mutation of `tools`. Subscribers
    /// (the gRPC `WatchTools` handler) hold a `watch::Receiver<u64>` and
    /// `.changed().await` to be woken when the registry changes.
    tools_revision: watch::Sender<u64>,
    chambers: RwLock<HashMap<String, Chamber>>,
    pending_calls: RwLock<HashMap<String, Vec<PendingCall>>>,
    call_notify: Notify,
    result_txs: RwLock<HashMap<String, ToolResultGuard>>,
    /// `call_id -> tool_name` shadow map populated alongside `result_txs`
    /// in `call_tool` and drained alongside `take_result_tx` in
    /// `send_tool_result`. Needed because the result RPC carries only
    /// `call_id`; the bump-last_activity step needs the tool_name to
    /// reach the right `ActiveJob` entry.
    call_id_to_tool: RwLock<HashMap<String, String>>,
    active_jobs: RwLock<HashMap<String, ActiveJob>>,
    /// Per-tool mutex map. Guards the get-probe-create-set sequence in
    /// `call_tool` so two concurrent CallTool RPCs for the same tool can
    /// not both observe `get_active_job=None` and both spawn Jobs. The
    /// outer `RwLock` only serializes map insertion; each inner
    /// `Arc<Mutex<()>>` is held across the whole dispatch sequence.
    tool_dispatch_locks: RwLock<HashMap<String, Arc<Mutex<()>>>>,
    kube_client: Option<kube::Client>,
    namespace: String,
    controller_addr: String,
    scheduling: SchedulingConfig,
}

impl ControllerState {
    pub fn new(
        kube_client: Option<kube::Client>,
        namespace: String,
        controller_addr: String,
        scheduling: SchedulingConfig,
    ) -> Arc<Self> {
        let (tools_revision, _) = watch::channel(0u64);
        Arc::new(Self {
            tools: RwLock::new(HashMap::new()),
            tools_revision,
            chambers: RwLock::new(HashMap::new()),
            pending_calls: RwLock::new(HashMap::new()),
            call_notify: Notify::new(),
            result_txs: RwLock::new(HashMap::new()),
            call_id_to_tool: RwLock::new(HashMap::new()),
            active_jobs: RwLock::new(HashMap::new()),
            tool_dispatch_locks: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            controller_addr,
            scheduling,
        })
    }

    /// Subscribe to tool-registry change notifications. Returns a receiver
    /// whose `.changed().await` resolves whenever `set_tools_for_chamber`,
    /// `remove_tools_for_chamber`, or `clear_tools` runs. The receiver yields
    /// the current revision number on `borrow()`; absolute value is opaque,
    /// only changes matter.
    pub fn subscribe_tools_revision(&self) -> watch::Receiver<u64> {
        self.tools_revision.subscribe()
    }

    pub fn kube_client(&self) -> Option<&kube::Client> {
        self.kube_client.as_ref()
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn controller_addr(&self) -> &str {
        &self.controller_addr
    }

    pub fn scheduling(&self) -> &SchedulingConfig {
        &self.scheduling
    }

    // -- Tool registry --

    pub async fn get_tool(&self, name: &str) -> Option<RegisteredTool> {
        self.tools.read().await.get(name).cloned()
    }

    pub async fn list_tools(&self) -> Vec<(String, RegisteredTool)> {
        self.tools
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub async fn list_tools_for_workspace(
        &self,
        workspace: &str,
        bindings: &WorkspaceBindings,
    ) -> Vec<(String, RegisteredTool)> {
        let chambers = bindings.chambers_for(workspace);
        if chambers.is_empty() {
            return vec![];
        }
        self.tools
            .read()
            .await
            .iter()
            .filter(|(_, tool)| chambers.iter().any(|c| c == &tool.chamber_name))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub async fn set_tools_for_chamber(&self, chamber_name: &str, tools: Vec<RegisteredTool>) {
        let mut registry = self.tools.write().await;
        registry.retain(|_, t| t.chamber_name != chamber_name);
        for tool in tools {
            if registry.contains_key(&tool.name) {
                warn!(
                    tool = %tool.name,
                    chamber = %chamber_name,
                    "duplicate tool name, first chamber wins"
                );
                continue;
            }
            registry.insert(tool.name.clone(), tool);
        }
        drop(registry);
        self.tools_revision.send_modify(|r| *r += 1);
    }

    pub async fn remove_tools_for_chamber(&self, chamber_name: &str) {
        self.tools
            .write()
            .await
            .retain(|_, t| t.chamber_name != chamber_name);
        self.tools_revision.send_modify(|r| *r += 1);
    }

    pub async fn clear_tools(&self) {
        self.tools.write().await.clear();
        self.tools_revision.send_modify(|r| *r += 1);
    }

    pub async fn tool_count(&self) -> usize {
        self.tools.read().await.len()
    }

    // -- Chamber registry --

    pub async fn get_chamber(&self, name: &str) -> Option<Chamber> {
        self.chambers.read().await.get(name).cloned()
    }

    pub async fn set_chamber(&self, name: String, chamber: Chamber) {
        self.chambers.write().await.insert(name, chamber);
    }

    pub async fn remove_chamber(&self, name: &str) {
        self.chambers.write().await.remove(name);
    }

    pub async fn clear_chambers(&self) {
        self.chambers.write().await.clear();
    }

    pub async fn chamber_count(&self) -> usize {
        self.chambers.read().await.len()
    }

    // -- Call queue --

    pub async fn enqueue_call(&self, call: PendingCall) {
        self.pending_calls
            .write()
            .await
            .entry(call.tool_name.clone())
            .or_default()
            .push(call);
        self.call_notify.notify_waiters();
    }

    pub async fn dequeue_call(&self, tool_name: &str) -> Option<PendingCall> {
        let mut pending = self.pending_calls.write().await;
        let calls = pending.get_mut(tool_name)?;
        if calls.is_empty() {
            None
        } else {
            Some(calls.remove(0))
        }
    }

    pub async fn wait_for_call(&self) {
        self.call_notify.notified().await;
    }

    // -- Result channels --

    pub async fn set_result_tx(
        &self,
        call_id: String,
        tool_name: String,
        tx: oneshot::Sender<ToolCallResult>,
    ) {
        self.result_txs
            .write()
            .await
            .insert(call_id.clone(), ToolResultGuard::new(tx));
        self.call_id_to_tool
            .write()
            .await
            .insert(call_id, tool_name);
    }

    /// Drains both the result channel and the `call_id -> tool_name`
    /// shadow entry in one shot. Returns the tool_name alongside the
    /// sender so the caller can `bump_last_activity` without a second
    /// lookup.
    pub async fn take_result_tx(&self, call_id: &str) -> Option<(ToolResultGuard, String)> {
        let tx = self.result_txs.write().await.remove(call_id)?;
        let tool_name = self
            .call_id_to_tool
            .write()
            .await
            .remove(call_id)
            .unwrap_or_default();
        Some((tx, tool_name))
    }

    /// Drain every pending result sender whose call is bound to
    /// `tool_name`, removing both the `result_txs` entry and its
    /// `call_id -> tool_name` shadow. Used by the reap path: dropping the
    /// returned guards fires each one's terminal error `ToolCallResult`,
    /// unblocking any `call_tool` parked on a chamber that was torn down.
    /// There is no tool_name -> call_id reverse index, so this scans the
    /// shadow map. Locks are taken sequentially (not nested), matching
    /// `set_result_tx`/`take_result_tx`, so it can't deadlock against them.
    pub async fn take_result_txs_for_tool(&self, tool_name: &str) -> Vec<ToolResultGuard> {
        let call_ids: Vec<String> = {
            let shadow = self.call_id_to_tool.read().await;
            shadow
                .iter()
                .filter(|(_, t)| t.as_str() == tool_name)
                .map(|(c, _)| c.clone())
                .collect()
        };
        let mut guards = Vec::with_capacity(call_ids.len());
        {
            let mut txs = self.result_txs.write().await;
            for call_id in &call_ids {
                if let Some(g) = txs.remove(call_id) {
                    guards.push(g);
                }
            }
        }
        {
            let mut shadow = self.call_id_to_tool.write().await;
            for call_id in &call_ids {
                shadow.remove(call_id);
            }
        }
        guards
    }

    // -- Active jobs (keepalive) --

    pub async fn list_active_jobs(&self) -> Vec<(String, String, u64, Instant)> {
        self.active_jobs
            .read()
            .await
            .iter()
            .map(|(name, job)| {
                (
                    name.clone(),
                    job.job_name.clone(),
                    job.keepalive_seconds,
                    job.last_activity,
                )
            })
            .collect()
    }

    pub async fn get_active_job(&self, name: &str) -> Option<ActiveJob> {
        self.active_jobs.read().await.get(name).cloned()
    }

    pub async fn set_active_job(&self, name: String, job: ActiveJob) {
        self.active_jobs.write().await.insert(name, job);
    }

    pub async fn remove_active_job(&self, name: &str) {
        self.active_jobs.write().await.remove(name);
    }

    /// Refresh the keepalive idle timer for a tool. No-op when the tool
    /// has no `ActiveJob` (caller didn't go through the spawn path, or
    /// the cleanup loop already reaped it).
    pub async fn bump_last_activity(&self, name: &str) {
        if let Some(j) = self.active_jobs.write().await.get_mut(name) {
            j.last_activity = Instant::now();
        }
    }

    pub async fn active_job_count(&self) -> usize {
        self.active_jobs.read().await.len()
    }

    /// Get-or-insert the per-tool dispatch mutex. The returned `Arc` is
    /// cheap to clone; callers `.lock().await` on it across the
    /// get-probe-create-set sequence in `call_tool`.
    pub async fn tool_dispatch_lock(&self, name: &str) -> Arc<Mutex<()>> {
        if let Some(m) = self.tool_dispatch_locks.read().await.get(name) {
            return m.clone();
        }
        let mut w = self.tool_dispatch_locks.write().await;
        w.entry(name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{Chamber, ChamberSpec};

    fn test_chamber(name: &str) -> Chamber {
        Chamber::new(
            name,
            ChamberSpec {
                image: None,
                credentials: vec![],
                egress: vec![],
                keepalive: false,
            },
        )
    }

    fn test_registered_tool(name: &str, chamber: &str) -> RegisteredTool {
        RegisteredTool {
            name: name.to_string(),
            chamber_name: chamber.to_string(),
            description: format!("Execute a {name} command."),
            image: "test:latest".to_string(),
            args: vec![],
        }
    }

    #[tokio::test]
    async fn tool_count_reflects_insertions() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        assert_eq!(state.tool_count().await, 0);
        state
            .set_tools_for_chamber(
                "c1",
                vec![
                    test_registered_tool("git", "c1"),
                    test_registered_tool("gh", "c1"),
                ],
            )
            .await;
        assert_eq!(state.tool_count().await, 2);
    }

    #[tokio::test]
    async fn clear_tools_empties_registry() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        state
            .set_tools_for_chamber("c1", vec![test_registered_tool("git", "c1")])
            .await;
        state.clear_tools().await;
        assert_eq!(state.tool_count().await, 0);
    }

    #[tokio::test]
    async fn set_tools_replaces_chamber_tools() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        state
            .set_tools_for_chamber("c1", vec![test_registered_tool("git", "c1")])
            .await;
        state
            .set_tools_for_chamber("c1", vec![test_registered_tool("gh", "c1")])
            .await;
        assert_eq!(state.tool_count().await, 1);
        assert!(state.get_tool("gh").await.is_some());
        assert!(state.get_tool("git").await.is_none());
    }

    #[tokio::test]
    async fn remove_tools_for_chamber_only_affects_that_chamber() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        state
            .set_tools_for_chamber("c1", vec![test_registered_tool("git", "c1")])
            .await;
        state
            .set_tools_for_chamber("c2", vec![test_registered_tool("gh", "c2")])
            .await;
        state.remove_tools_for_chamber("c1").await;
        assert_eq!(state.tool_count().await, 1);
        assert!(state.get_tool("gh").await.is_some());
    }

    #[tokio::test]
    async fn duplicate_tool_name_first_chamber_wins() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        state
            .set_tools_for_chamber("c1", vec![test_registered_tool("git", "c1")])
            .await;
        state
            .set_tools_for_chamber("c2", vec![test_registered_tool("git", "c2")])
            .await;
        assert_eq!(state.tool_count().await, 1);
        let tool = state.get_tool("git").await.unwrap();
        assert_eq!(tool.chamber_name, "c1");
    }

    #[tokio::test]
    async fn chamber_count_reflects_insertions() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        assert_eq!(state.chamber_count().await, 0);
        state.set_chamber("a".into(), test_chamber("a")).await;
        state.set_chamber("b".into(), test_chamber("b")).await;
        assert_eq!(state.chamber_count().await, 2);
    }

    #[tokio::test]
    async fn clear_chambers_empties_registry() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        state.set_chamber("a".into(), test_chamber("a")).await;
        state.clear_chambers().await;
        assert_eq!(state.chamber_count().await, 0);
    }

    #[tokio::test]
    async fn wait_for_call_blocks_until_notify() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        let state2 = state.clone();

        let wait_handle = tokio::spawn(async move {
            state2.wait_for_call().await;
        });

        tokio::task::yield_now().await;
        assert!(!wait_handle.is_finished(), "should be blocking");

        state
            .enqueue_call(PendingCall {
                call_id: "c".into(),
                tool_name: "t".into(),
                args: HashMap::new(),
                working_dir: "/w".into(),
            })
            .await;

        tokio::time::timeout(std::time::Duration::from_secs(2), wait_handle)
            .await
            .expect("wait_for_call should unblock")
            .unwrap();
    }

    #[test]
    fn bindings_has_chamber_true_for_bound() {
        let mut map = HashMap::new();
        map.insert(
            "ws1".to_string(),
            vec!["git".to_string(), "ssh".to_string()],
        );
        let bindings = WorkspaceBindings { map };
        assert!(bindings.has_chamber("ws1", "git"));
        assert!(bindings.has_chamber("ws1", "ssh"));
    }

    #[test]
    fn bindings_has_chamber_false_for_unbound() {
        let mut map = HashMap::new();
        map.insert("ws1".to_string(), vec!["git".to_string()]);
        let bindings = WorkspaceBindings { map };
        assert!(!bindings.has_chamber("ws1", "ssh"));
    }

    #[test]
    fn bindings_has_chamber_false_for_unknown_workspace() {
        let bindings = WorkspaceBindings::empty();
        assert!(!bindings.has_chamber("nonexistent", "git"));
    }

    #[test]
    fn bindings_chambers_for_unknown_returns_empty() {
        let bindings = WorkspaceBindings::empty();
        assert!(bindings.chambers_for("nonexistent").is_empty());
    }

    #[tokio::test]
    async fn list_tools_for_workspace_filters_by_binding() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        state
            .set_tools_for_chamber("git", vec![test_registered_tool("git-push", "git")])
            .await;
        state
            .set_tools_for_chamber("ssh", vec![test_registered_tool("ssh-exec", "ssh")])
            .await;

        let mut map = HashMap::new();
        map.insert("ws1".to_string(), vec!["git".to_string()]);
        let bindings = WorkspaceBindings { map };

        let tools = state.list_tools_for_workspace("ws1", &bindings).await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0, "git-push");
    }

    #[tokio::test]
    async fn set_tools_for_chamber_bumps_revision() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        let mut rx = state.subscribe_tools_revision();
        // Drain any initial value already in the channel before mutation.
        rx.mark_unchanged();
        state
            .set_tools_for_chamber("c1", vec![test_registered_tool("git", "c1")])
            .await;
        // .changed() resolves immediately because send_modify fired.
        tokio::time::timeout(std::time::Duration::from_millis(100), rx.changed())
            .await
            .expect("revision must change after set_tools_for_chamber")
            .expect("sender must still be alive");
    }

    #[tokio::test]
    async fn remove_tools_for_chamber_bumps_revision() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        state
            .set_tools_for_chamber("c1", vec![test_registered_tool("git", "c1")])
            .await;
        let mut rx = state.subscribe_tools_revision();
        rx.mark_unchanged();
        state.remove_tools_for_chamber("c1").await;
        tokio::time::timeout(std::time::Duration::from_millis(100), rx.changed())
            .await
            .expect("revision must change after remove_tools_for_chamber")
            .expect("sender must still be alive");
    }

    #[tokio::test]
    async fn clear_tools_bumps_revision() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        let mut rx = state.subscribe_tools_revision();
        rx.mark_unchanged();
        state.clear_tools().await;
        tokio::time::timeout(std::time::Duration::from_millis(100), rx.changed())
            .await
            .expect("revision must change after clear_tools")
            .expect("sender must still be alive");
    }

    #[tokio::test]
    async fn get_active_job_returns_set_value() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        state
            .set_active_job(
                "Search".into(),
                ActiveJob {
                    job_name: "airlock-search-abc".into(),
                    tool_name: "Search".into(),
                    last_activity: Instant::now(),
                    keepalive_seconds: 600,
                },
            )
            .await;

        let got = state.get_active_job("Search").await.expect("present");
        assert_eq!(got.job_name, "airlock-search-abc");
        assert_eq!(got.tool_name, "Search");
        assert_eq!(got.keepalive_seconds, 600);
        assert!(state.get_active_job("absent").await.is_none());
    }

    #[tokio::test]
    async fn bump_last_activity_updates_timestamp() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        let started = Instant::now() - std::time::Duration::from_secs(10);
        state
            .set_active_job(
                "Shell".into(),
                ActiveJob {
                    job_name: "airlock-shell-abc".into(),
                    tool_name: "Shell".into(),
                    last_activity: started,
                    keepalive_seconds: 600,
                },
            )
            .await;

        state.bump_last_activity("Shell").await;

        let got = state.get_active_job("Shell").await.unwrap();
        assert!(
            got.last_activity > started,
            "last_activity must advance on bump"
        );

        // Absent key is a no-op (does not panic, does not insert).
        state.bump_last_activity("Nope").await;
        assert!(state.get_active_job("Nope").await.is_none());
    }

    #[tokio::test]
    async fn tool_dispatch_lock_returns_same_mutex_per_tool() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        let a = state.tool_dispatch_lock("Search").await;
        let b = state.tool_dispatch_lock("Search").await;
        let c = state.tool_dispatch_lock("Read").await;

        // Same tool → same underlying Mutex (Arc::ptr_eq).
        assert!(Arc::ptr_eq(&a, &b));
        // Different tool → distinct Mutex.
        assert!(!Arc::ptr_eq(&a, &c));

        // Holding the lock blocks a second acquire on the same Arc.
        let _g = a.lock().await;
        assert!(b.try_lock().is_err());
    }

    #[tokio::test]
    async fn set_take_result_tx_round_trips_tool_name() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        let (tx, _rx) = oneshot::channel::<ToolCallResult>();
        state
            .set_result_tx("call-1".into(), "Search".into(), tx)
            .await;

        let (_tx_back, tool_name) = state.take_result_tx("call-1").await.expect("present");
        assert_eq!(tool_name, "Search");

        // Second take returns None (both maps drained).
        assert!(state.take_result_tx("call-1").await.is_none());
    }

    #[tokio::test]
    async fn list_tools_for_workspace_unknown_returns_empty() {
        let state = ControllerState::new(
            None,
            String::new(),
            String::new(),
            SchedulingConfig::default(),
        );
        state
            .set_tools_for_chamber("git", vec![test_registered_tool("git-push", "git")])
            .await;

        let bindings = WorkspaceBindings::empty();
        let tools = state.list_tools_for_workspace("unknown", &bindings).await;
        assert!(tools.is_empty());
    }
}
