use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use shared::scheduling::SchedulingConfig;
use tokio::sync::{oneshot, Notify, RwLock};
use tracing::warn;

use crate::crd::AirlockChamber;

fn clone_tool_entry((k, v): (&String, &RegisteredTool)) -> (String, RegisteredTool) {
    (
        k.clone(),
        RegisteredTool {
            name: v.name.clone(),
            chamber_name: v.chamber_name.clone(),
            description: v.description.clone(),
            image: v.image.clone(),
        },
    )
}

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

pub struct RegisteredTool {
    pub name: String,
    pub chamber_name: String,
    pub description: String,
    pub image: String,
}

pub struct ToolCallResult {
    pub output: String,
    pub is_error: bool,
    pub exit_code: i32,
}

pub struct PendingCall {
    pub call_id: String,
    pub tool_name: String,
    pub input_json: String,
    pub command_template: String,
    pub working_dir: String,
}

pub struct ActiveJob {
    pub job_name: String,
    pub tool_name: String,
    pub last_activity: Instant,
    pub keepalive_seconds: u64,
}

pub struct ControllerState {
    tools: RwLock<HashMap<String, RegisteredTool>>,
    chambers: RwLock<HashMap<String, AirlockChamber>>,
    pending_calls: RwLock<HashMap<String, Vec<PendingCall>>>,
    call_notify: Notify,
    result_txs: RwLock<HashMap<String, oneshot::Sender<ToolCallResult>>>,
    active_jobs: RwLock<HashMap<String, ActiveJob>>,
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
        Arc::new(Self {
            tools: RwLock::new(HashMap::new()),
            chambers: RwLock::new(HashMap::new()),
            pending_calls: RwLock::new(HashMap::new()),
            call_notify: Notify::new(),
            result_txs: RwLock::new(HashMap::new()),
            active_jobs: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            controller_addr,
            scheduling,
        })
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

    pub async fn get_tool(&self, name: &str) -> Option<(String, String, String)> {
        let tools = self.tools.read().await;
        tools.get(name).map(|t| {
            (
                t.chamber_name.clone(),
                t.image.clone(),
                t.description.clone(),
            )
        })
    }

    pub async fn list_tools(&self) -> Vec<(String, RegisteredTool)> {
        self.tools
            .read()
            .await
            .iter()
            .map(clone_tool_entry)
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
            .map(clone_tool_entry)
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
    }

    pub async fn remove_tools_for_chamber(&self, chamber_name: &str) {
        self.tools
            .write()
            .await
            .retain(|_, t| t.chamber_name != chamber_name);
    }

    pub async fn clear_tools(&self) {
        self.tools.write().await.clear();
    }

    pub async fn tool_count(&self) -> usize {
        self.tools.read().await.len()
    }

    // -- Chamber registry --

    pub async fn get_chamber(&self, name: &str) -> Option<AirlockChamber> {
        self.chambers.read().await.get(name).cloned()
    }

    pub async fn set_chamber(&self, name: String, chamber: AirlockChamber) {
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

    pub async fn set_result_tx(&self, call_id: String, tx: oneshot::Sender<ToolCallResult>) {
        self.result_txs.write().await.insert(call_id, tx);
    }

    pub async fn take_result_tx(&self, call_id: &str) -> Option<oneshot::Sender<ToolCallResult>> {
        self.result_txs.write().await.remove(call_id)
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

    pub async fn set_active_job(&self, name: String, job: ActiveJob) {
        self.active_jobs.write().await.insert(name, job);
    }

    pub async fn remove_active_job(&self, name: &str) {
        self.active_jobs.write().await.remove(name);
    }

    pub async fn active_job_count(&self) -> usize {
        self.active_jobs.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{AirlockChamber, AirlockChamberSpec};

    fn test_chamber(name: &str) -> AirlockChamber {
        AirlockChamber::new(
            name,
            AirlockChamberSpec {
                image: None,
                workspace_mode: "readWrite".to_string(),
                workspace_mount_path: "/workspace".to_string(),
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
        let (chamber, _, _) = state.get_tool("git").await.unwrap();
        assert_eq!(chamber, "c1");
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
                input_json: "{}".into(),
                command_template: "cmd".into(),
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
