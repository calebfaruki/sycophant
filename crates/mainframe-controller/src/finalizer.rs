//! Workspace deletion finalizer. K8s' ownerRef cascade GCs the
//! workspace Pod when the Workspace is deleted, but the Pod tears down
//! asynchronously. The finalizer ensures `kubectl delete workspace foo`
//! doesn't report success until the Pod is gone — bounding the
//! "ghost pod" window operators see between `kubectl delete` and pod
//! termination.

use std::sync::Arc;

use kube::api::{Api, Patch, PatchParams};
use kube::Client;
use serde_json::json;
use tracing::info;

use crate::materialize::pod_child_exists;
use crate::workspace_crd::Workspace;

/// Finalizer name. The `sandbox-cleanup` suffix is preserved verbatim
/// even though the child is now a Pod, because the string is part of
/// the live-resource contract: existing Workspaces in deployed clusters
/// carry it in `metadata.finalizers`, and renaming would strand them.
pub const FINALIZER_NAME: &str = "workspaces.sycophant.md/sandbox-cleanup";

/// True when the Workspace's `metadata.finalizers` already contains
/// our finalizer.
pub fn has_finalizer(workspace: &Workspace) -> bool {
    workspace
        .metadata
        .finalizers
        .as_ref()
        .is_some_and(|fs| fs.iter().any(|f| f == FINALIZER_NAME))
}

/// Add the finalizer name to a list, preserving order and avoiding
/// duplicates. Pure for testability.
pub fn with_finalizer_added(current: Option<&Vec<String>>) -> Vec<String> {
    let mut updated: Vec<String> = current.cloned().unwrap_or_default();
    if !updated.iter().any(|f| f == FINALIZER_NAME) {
        updated.push(FINALIZER_NAME.to_string());
    }
    updated
}

/// Drop the finalizer name from a list, preserving order. Pure for
/// testability.
pub fn with_finalizer_removed(current: &[String]) -> Vec<String> {
    current
        .iter()
        .filter(|f| *f != FINALIZER_NAME)
        .cloned()
        .collect()
}

/// SSA-patch the Workspace's `metadata.finalizers` to include our
/// finalizer. No-op if already present. Called on every reconcile of a
/// not-yet-deleted Workspace so a controller restart can recover.
pub async fn ensure_finalizer(
    client: &Client,
    namespace: &str,
    workspace: &Workspace,
) -> anyhow::Result<()> {
    if has_finalizer(workspace) {
        return Ok(());
    }
    let name = workspace.metadata.name.clone().unwrap_or_default();
    let updated = with_finalizer_added(workspace.metadata.finalizers.as_ref());
    let api: Api<Workspace> = Api::namespaced(client.clone(), namespace);
    let pp = PatchParams::apply(crate::materialize::FIELD_MANAGER).force();
    // SSA requires apiVersion + kind in the body; without them
    // kube-apiserver rejects with "invalid object type: /, Kind=".
    let patch = json!({
        "apiVersion": "sycophant.md/v1",
        "kind": "Workspace",
        "metadata": { "name": name, "finalizers": updated }
    });
    api.patch(&name, &pp, &Patch::Apply(&patch)).await?;
    info!(workspace = %name, "finalizer added");
    Ok(())
}

/// Outcome of a deletion-time reconcile. The watcher uses this to
/// decide whether to keep retrying or move on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeletionStep {
    /// Workspace Pod still present; controller should requeue and check
    /// again after a short delay.
    WaitForPod,
    /// Workspace Pod confirmed gone; finalizer was removed from the
    /// Workspace and K8s will now complete the delete.
    FinalizerRemoved,
}

/// Run one tick of the deletion path. Idempotent — repeated invocations
/// while the Pod still exists return `WaitForPod`; once the Pod is gone
/// the finalizer is patched off and we return `FinalizerRemoved`.
pub async fn process_deletion(
    client: &Client,
    namespace: &str,
    workspace: &Workspace,
) -> anyhow::Result<DeletionStep> {
    let name = workspace.metadata.name.clone().unwrap_or_default();
    let child_present = pod_child_exists(client, namespace, &name).await?;
    if child_present {
        info!(
            workspace = %name,
            "deletion waiting on pod cleanup"
        );
        return Ok(DeletionStep::WaitForPod);
    }

    let current = workspace.metadata.finalizers.clone().unwrap_or_default();
    let updated = with_finalizer_removed(&current);
    let api: Api<Workspace> = Api::namespaced(client.clone(), namespace);
    let pp = PatchParams::apply(crate::materialize::FIELD_MANAGER).force();
    let patch = json!({
        "apiVersion": "sycophant.md/v1",
        "kind": "Workspace",
        "metadata": { "name": name, "finalizers": updated }
    });
    api.patch(&name, &pp, &Patch::Apply(&patch)).await?;
    info!(workspace = %name, "finalizer removed; pod confirmed gone");
    Ok(DeletionStep::FinalizerRemoved)
}

/// Periodic requeue interval used by the watcher when a deletion is in
/// flight and the Pod still exists. Short enough that operators don't
/// see a multi-second pause between `kubectl delete` returning and the
/// Workspace actually disappearing, long enough to not hammer the K8s API.
pub fn deletion_requeue_delay() -> std::time::Duration {
    std::time::Duration::from_secs(2)
}

/// Marker type kept here so tests can assert on the controller-side
/// state when wiring it through the watcher (Stage 3+). Currently
/// unused at module scope but retained to make the import shape
/// explicit.
#[allow(dead_code)]
struct _UnusedStateMarker(Arc<()>);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_crd::WorkspaceSpec;

    fn empty_workspace() -> Workspace {
        Workspace::new(
            "demo",
            WorkspaceSpec {
                image: "img".into(),
                tag: "t".into(),
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

    #[test]
    fn has_finalizer_false_when_metadata_finalizers_empty() {
        let w = empty_workspace();
        assert!(!has_finalizer(&w));
    }

    #[test]
    fn has_finalizer_false_when_metadata_finalizers_lacks_ours() {
        let mut w = empty_workspace();
        w.metadata.finalizers = Some(vec!["other.example.com/wait".into()]);
        assert!(!has_finalizer(&w));
    }

    #[test]
    fn has_finalizer_true_when_metadata_finalizers_contains_ours() {
        let mut w = empty_workspace();
        w.metadata.finalizers = Some(vec![FINALIZER_NAME.into()]);
        assert!(has_finalizer(&w));
    }

    #[test]
    fn with_finalizer_added_to_empty_list() {
        let result = with_finalizer_added(None);
        assert_eq!(result, vec![FINALIZER_NAME.to_string()]);
    }

    #[test]
    fn with_finalizer_added_idempotent_when_already_present() {
        let existing = vec![FINALIZER_NAME.to_string()];
        let result = with_finalizer_added(Some(&existing));
        assert_eq!(result, vec![FINALIZER_NAME.to_string()]);
        assert_eq!(result.len(), 1, "no duplicates added");
    }

    #[test]
    fn with_finalizer_added_preserves_other_finalizers() {
        let existing = vec!["other.example.com/wait".to_string()];
        let result = with_finalizer_added(Some(&existing));
        assert_eq!(
            result,
            vec![
                "other.example.com/wait".to_string(),
                FINALIZER_NAME.to_string()
            ]
        );
    }

    #[test]
    fn with_finalizer_removed_drops_ours() {
        let existing = vec![
            FINALIZER_NAME.to_string(),
            "other.example.com/wait".to_string(),
        ];
        let result = with_finalizer_removed(&existing);
        assert_eq!(result, vec!["other.example.com/wait".to_string()]);
    }

    #[test]
    fn with_finalizer_removed_is_noop_when_ours_absent() {
        let existing = vec!["other.example.com/wait".to_string()];
        let result = with_finalizer_removed(&existing);
        assert_eq!(result, vec!["other.example.com/wait".to_string()]);
    }

    #[test]
    fn with_finalizer_removed_handles_empty_list() {
        let existing: Vec<String> = vec![];
        let result = with_finalizer_removed(&existing);
        assert!(result.is_empty());
    }

    #[test]
    fn deletion_requeue_delay_is_reasonable_for_operator_ux() {
        let d = deletion_requeue_delay();
        // Pinning the contract: between 1 and 5 seconds. Lower bound
        // keeps API server load reasonable; upper bound keeps
        // `kubectl delete workspace` from feeling sluggish.
        assert!(d >= std::time::Duration::from_secs(1));
        assert!(d <= std::time::Duration::from_secs(5));
    }
}
