use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::crd::Kernel;
use crate::crd::Workspace;

/// Shared in-memory state for the mainframe controller. Holds the
/// observed Kernel and Workspace CRs plus their last-seen generations
/// (used for Apply-event dedup). Both kinds use the same shape but
/// separate maps so renames or recreations under the same name don't
/// cross-pollinate.
pub struct ControllerState {
    kernels: RwLock<HashMap<String, Kernel>>,
    last_kernel_generations: RwLock<HashMap<String, i64>>,
    workspaces: RwLock<HashMap<String, Workspace>>,
    last_workspace_generations: RwLock<HashMap<String, i64>>,
}

impl ControllerState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            kernels: RwLock::new(HashMap::new()),
            last_kernel_generations: RwLock::new(HashMap::new()),
            workspaces: RwLock::new(HashMap::new()),
            last_workspace_generations: RwLock::new(HashMap::new()),
        })
    }

    // --- Kernel ---

    pub async fn set_kernel(&self, name: String, kernel: Kernel) {
        self.kernels.write().await.insert(name, kernel);
    }

    pub async fn get_kernel(&self, name: &str) -> Option<Kernel> {
        self.kernels.read().await.get(name).cloned()
    }

    pub async fn remove_kernel(&self, name: &str) {
        self.kernels.write().await.remove(name);
        self.last_kernel_generations.write().await.remove(name);
    }

    pub async fn list_kernel_names(&self) -> Vec<String> {
        self.kernels.read().await.keys().cloned().collect()
    }

    pub async fn kernel_count(&self) -> usize {
        self.kernels.read().await.len()
    }

    pub async fn record_kernel_generation(&self, name: &str, generation: i64) {
        self.last_kernel_generations
            .write()
            .await
            .insert(name.to_string(), generation);
    }

    pub async fn last_kernel_generation(&self, name: &str) -> Option<i64> {
        self.last_kernel_generations.read().await.get(name).copied()
    }

    pub async fn clear_kernels(&self) {
        self.kernels.write().await.clear();
        self.last_kernel_generations.write().await.clear();
    }

    // --- Workspace ---

    pub async fn set_workspace(&self, name: String, workspace: Workspace) {
        self.workspaces.write().await.insert(name, workspace);
    }

    pub async fn get_workspace(&self, name: &str) -> Option<Workspace> {
        self.workspaces.read().await.get(name).cloned()
    }

    pub async fn remove_workspace(&self, name: &str) {
        self.workspaces.write().await.remove(name);
        self.last_workspace_generations.write().await.remove(name);
    }

    pub async fn list_workspace_names(&self) -> Vec<String> {
        self.workspaces.read().await.keys().cloned().collect()
    }

    pub async fn workspace_count(&self) -> usize {
        self.workspaces.read().await.len()
    }

    pub async fn record_workspace_generation(&self, name: &str, generation: i64) {
        self.last_workspace_generations
            .write()
            .await
            .insert(name.to_string(), generation);
    }

    pub async fn last_workspace_generation(&self, name: &str) -> Option<i64> {
        self.last_workspace_generations
            .read()
            .await
            .get(name)
            .copied()
    }

    pub async fn clear_workspaces(&self) {
        self.workspaces.write().await.clear();
        self.last_workspace_generations.write().await.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::KernelSpec;
    use crate::crd::WorkspaceSpec;
    use shared::storage::HostPathSpec;

    fn test_kernel(name: &str) -> Kernel {
        Kernel::new(
            name,
            KernelSpec {
                kind: "HostPath".into(),
                host_path: Some(HostPathSpec {
                    path: format!("/host/sycophant/{name}"),
                }),
                s3: None,
            },
        )
    }

    fn test_workspace(name: &str) -> Workspace {
        Workspace::new(
            name,
            WorkspaceSpec {
                image: "ghcr.io/calebfaruki/transponder".into(),
                tag: "v0.1".into(),
                pull_policy: None,
                cpu: None,
                memory: None,
                storage: None,
                mainframe: None,
                kernels: vec![],
                chambers: vec![],
            },
        )
    }

    #[tokio::test]
    async fn kernel_count_reflects_insertions() {
        let state = ControllerState::new();
        assert_eq!(state.kernel_count().await, 0);
        state
            .set_kernel("default".into(), test_kernel("default"))
            .await;
        assert_eq!(state.kernel_count().await, 1);
    }

    #[tokio::test]
    async fn remove_kernel_drops_state_and_generation() {
        let state = ControllerState::new();
        state
            .set_kernel("default".into(), test_kernel("default"))
            .await;
        state.record_kernel_generation("default", 7).await;
        state.remove_kernel("default").await;
        assert_eq!(state.kernel_count().await, 0);
        assert!(state.last_kernel_generation("default").await.is_none());
    }

    #[tokio::test]
    async fn kernel_generation_round_trip() {
        let state = ControllerState::new();
        state.record_kernel_generation("default", 42).await;
        assert_eq!(state.last_kernel_generation("default").await, Some(42));
    }

    #[tokio::test]
    async fn clear_kernels_empties_only_kernel_state() {
        let state = ControllerState::new();
        state.set_kernel("a".into(), test_kernel("a")).await;
        state.set_workspace("w".into(), test_workspace("w")).await;
        state.record_kernel_generation("a", 1).await;
        state.record_workspace_generation("w", 2).await;
        state.clear_kernels().await;
        assert_eq!(state.kernel_count().await, 0);
        assert!(state.last_kernel_generation("a").await.is_none());
        // workspace state untouched
        assert_eq!(state.workspace_count().await, 1);
        assert_eq!(state.last_workspace_generation("w").await, Some(2));
    }

    #[tokio::test]
    async fn list_kernel_names_returns_inserted_keys() {
        let state = ControllerState::new();
        state.set_kernel("alpha".into(), test_kernel("alpha")).await;
        state.set_kernel("beta".into(), test_kernel("beta")).await;
        let mut names = state.list_kernel_names().await;
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[tokio::test]
    async fn list_kernel_names_empty_when_no_kernels() {
        let state = ControllerState::new();
        assert!(state.list_kernel_names().await.is_empty());
    }

    #[tokio::test]
    async fn workspace_count_reflects_insertions() {
        let state = ControllerState::new();
        assert_eq!(state.workspace_count().await, 0);
        state
            .set_workspace("default".into(), test_workspace("default"))
            .await;
        assert_eq!(state.workspace_count().await, 1);
    }

    #[tokio::test]
    async fn remove_workspace_drops_state_and_generation() {
        let state = ControllerState::new();
        state
            .set_workspace("default".into(), test_workspace("default"))
            .await;
        state.record_workspace_generation("default", 11).await;
        state.remove_workspace("default").await;
        assert_eq!(state.workspace_count().await, 0);
        assert!(state.last_workspace_generation("default").await.is_none());
    }

    #[tokio::test]
    async fn workspace_generation_round_trip() {
        let state = ControllerState::new();
        state.record_workspace_generation("default", 13).await;
        assert_eq!(state.last_workspace_generation("default").await, Some(13));
    }

    #[tokio::test]
    async fn list_workspace_names_returns_inserted_keys() {
        let state = ControllerState::new();
        state
            .set_workspace("alpha".into(), test_workspace("alpha"))
            .await;
        state
            .set_workspace("beta".into(), test_workspace("beta"))
            .await;
        let mut names = state.list_workspace_names().await;
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[tokio::test]
    async fn list_workspace_names_empty_when_no_workspaces() {
        let state = ControllerState::new();
        assert!(state.list_workspace_names().await.is_empty());
    }

    #[tokio::test]
    async fn clear_workspaces_empties_only_workspace_state() {
        let state = ControllerState::new();
        state.set_kernel("a".into(), test_kernel("a")).await;
        state.set_workspace("w".into(), test_workspace("w")).await;
        state.record_kernel_generation("a", 1).await;
        state.record_workspace_generation("w", 2).await;
        state.clear_workspaces().await;
        assert_eq!(state.workspace_count().await, 0);
        assert!(state.last_workspace_generation("w").await.is_none());
        // kernel state untouched
        assert_eq!(state.kernel_count().await, 1);
        assert_eq!(state.last_kernel_generation("a").await, Some(1));
    }

    #[tokio::test]
    async fn workspace_and_kernel_generations_do_not_collide_by_name() {
        // A workspace named "foo" and a kernel named "foo" track
        // generations independently; updating one must not affect the
        // other.
        let state = ControllerState::new();
        state.record_kernel_generation("foo", 3).await;
        state.record_workspace_generation("foo", 7).await;
        assert_eq!(state.last_kernel_generation("foo").await, Some(3));
        assert_eq!(state.last_workspace_generation("foo").await, Some(7));
    }
}
