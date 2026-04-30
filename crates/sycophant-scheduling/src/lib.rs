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

    pub fn is_empty(&self) -> bool {
        self.node_selector.is_empty() && self.tolerations.is_empty()
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
  sycophant.io/workload: tightbeam
tolerations:
  - key: sycophant.io/workload
    operator: Equal
    value: tightbeam
    effect: NoSchedule
"#,
        )
        .unwrap();

        let config = SchedulingConfig::load(tmp.path().to_str().unwrap()).unwrap();
        assert!(!config.is_empty());
        assert_eq!(
            config.node_selector.get("sycophant.io/workload"),
            Some(&"tightbeam".to_string())
        );
        assert_eq!(config.tolerations.len(), 1);
        assert_eq!(
            config.tolerations[0].key.as_deref(),
            Some("sycophant.io/workload")
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
