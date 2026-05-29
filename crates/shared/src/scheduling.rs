use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Toleration;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SchedulingConfig {
    #[serde(default)]
    pub node_selector: BTreeMap<String, String>,
    #[serde(default)]
    pub tolerations: Vec<Toleration>,
}

impl SchedulingConfig {
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read scheduling config {path}: {e}"))?;
        serde_yaml::from_str(&content).map_err(|e| format!("failed to parse scheduling YAML: {e}"))
    }

    pub fn load_or_default(path: &str, has_kube: bool) -> Result<Self, String> {
        if !has_kube {
            tracing::info!("no kube client, scheduling config skipped");
            return Ok(Self::default());
        }
        match Self::load(path) {
            Ok(s) => {
                tracing::info!(path, "loaded scheduling config");
                Ok(s)
            }
            Err(e) => Err(format!("scheduling config required in-cluster: {e}")),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.node_selector.is_empty() && self.tolerations.is_empty()
    }
}

/// Test fixtures for `SchedulingConfig`-shaped assertions in workspace
/// crates. Gated by `#[cfg(any(test, feature = "test-fixtures"))]`: the
/// crate's own tests see this directly, downstream test code activates the
/// `test-fixtures` feature in its dev-dependency on `shared`.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod testing {
    use super::SchedulingConfig;
    use k8s_openapi::api::core::v1::{PodSpec, Toleration};
    use std::collections::BTreeMap;

    pub fn no_scheduling() -> SchedulingConfig {
        SchedulingConfig::default()
    }

    pub fn test_scheduling(workload: &str) -> SchedulingConfig {
        SchedulingConfig {
            node_selector: BTreeMap::from([("sycophant.md/workload".into(), workload.into())]),
            tolerations: vec![Toleration {
                key: Some("sycophant.md/workload".into()),
                operator: Some("Equal".into()),
                value: Some(workload.into()),
                effect: Some("NoSchedule".into()),
                ..Default::default()
            }],
        }
    }

    pub fn assert_scheduling(pod_spec: &PodSpec, workload: &str) {
        let ns = pod_spec
            .node_selector
            .as_ref()
            .expect("node_selector must be set");
        assert_eq!(ns.get("sycophant.md/workload"), Some(&workload.to_string()));
        assert_eq!(ns.len(), 1);

        let tols = pod_spec
            .tolerations
            .as_ref()
            .expect("tolerations must be set");
        assert_eq!(tols.len(), 1);
        assert_eq!(tols[0].key.as_deref(), Some("sycophant.md/workload"));
        assert_eq!(tols[0].value.as_deref(), Some(workload));
        assert_eq!(tols[0].operator.as_deref(), Some("Equal"));
        assert_eq!(tols[0].effect.as_deref(), Some("NoSchedule"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty() {
        let config = SchedulingConfig::default();
        assert!(config.is_empty());
        assert!(config.node_selector.is_empty());
        assert!(config.tolerations.is_empty());
    }

    #[test]
    fn load_from_yaml() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
node_selector:
  sycophant.md/workload: tightbeam
tolerations:
  - key: sycophant.md/workload
    operator: Equal
    value: tightbeam
    effect: NoSchedule
"#,
        )
        .unwrap();

        let config = SchedulingConfig::load(tmp.path().to_str().unwrap()).unwrap();
        assert!(!config.is_empty());
        assert_eq!(
            config.node_selector.get("sycophant.md/workload"),
            Some(&"tightbeam".to_string())
        );
        assert_eq!(config.tolerations.len(), 1);
        assert_eq!(
            config.tolerations[0].key.as_deref(),
            Some("sycophant.md/workload")
        );
        assert_eq!(config.tolerations[0].value.as_deref(), Some("tightbeam"));
        assert_eq!(config.tolerations[0].operator.as_deref(), Some("Equal"));
        assert_eq!(config.tolerations[0].effect.as_deref(), Some("NoSchedule"));
    }

    #[test]
    fn load_missing_file_returns_error() {
        let result = SchedulingConfig::load("/nonexistent/path.yaml");
        assert!(result.is_err());
    }

    #[test]
    fn load_empty_yaml_gives_defaults() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "{}").unwrap();
        let config = SchedulingConfig::load(tmp.path().to_str().unwrap()).unwrap();
        assert!(config.is_empty());
    }

    #[test]
    fn load_or_default_skips_when_no_kube() {
        let config = SchedulingConfig::load_or_default("/nonexistent/path.yaml", false).unwrap();
        assert!(config.is_empty());
    }

    #[test]
    fn load_or_default_errors_in_cluster_when_missing() {
        let result = SchedulingConfig::load_or_default("/nonexistent/path.yaml", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("required in-cluster"));
    }

    #[test]
    fn is_empty_false_with_node_selector() {
        let config = SchedulingConfig {
            node_selector: BTreeMap::from([("k".into(), "v".into())]),
            tolerations: vec![],
        };
        assert!(!config.is_empty());
    }

    #[test]
    fn is_empty_false_with_tolerations() {
        let config = SchedulingConfig {
            node_selector: BTreeMap::new(),
            tolerations: vec![Toleration::default()],
        };
        assert!(!config.is_empty());
    }
}
