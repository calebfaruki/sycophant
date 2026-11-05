//! The per-agent toolset grants the gateway serves on ListGrants. Names
//! only: the relay is the internet-facing pod, so no grant's Secret name, mount
//! path, or egress domain is ever rendered into what it reads.
//!
//! Merged from the per-agent `ag-<agent>-relay-grants` ConfigMaps
//! labelled `sycophant.md/type: relay-toolset-grants`.

use std::collections::{BTreeMap, HashMap};

use k8s_openapi::api::core::v1::ConfigMap;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use shared::configmap_watcher::merge_configmaps;

/// The `data` key each per-agent ConfigMap carries its bindings block under.
pub const GRANTS_DATA_KEY: &str = "grants.yaml";

/// One agent's toolset list entry as the bindings file writes it: a bare
/// toolset name, or a named entry carrying its grants. Grant specs beyond
/// the name (secret, path, egress) belong to the toolset controller; only the
/// keys matter here.
enum BindingEntry {
    Bare,
    Granted {
        name: String,
        grants: BTreeMap<String, serde_yaml::Value>,
    },
}

#[derive(Deserialize)]
struct RawGrantedEntry {
    name: String,
    #[serde(default)]
    grants: BTreeMap<String, serde_yaml::Value>,
}

/// A YAML string is a bare entry and a mapping is a grant-bearing one. Written
/// by hand rather than derived `untagged`, which would let an entry whose
/// `grants` is malformed fall through to the bare variant and vanish from the
/// grants instead of failing the load.
impl<'de> Deserialize<'de> for BindingEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match serde_yaml::Value::deserialize(deserializer)? {
            serde_yaml::Value::String(_) => Ok(BindingEntry::Bare),
            other => {
                let entry: RawGrantedEntry =
                    serde_yaml::from_value(other).map_err(D::Error::custom)?;
                Ok(BindingEntry::Granted {
                    name: entry.name,
                    grants: entry.grants,
                })
            }
        }
    }
}

/// agent → (toolset, grant names) pairs, in file order; grant names in
/// name order.
#[derive(Debug, Default, Clone)]
pub struct CapabilityGrants {
    map: HashMap<String, Vec<(String, Vec<String>)>>,
}

impl CapabilityGrants {
    /// Merge every per-agent ConfigMap into one table.
    pub fn merge_configmaps<'a, I>(cms: I) -> (Self, Vec<String>)
    where
        I: IntoIterator<Item = &'a ConfigMap>,
    {
        let (map, errors) =
            merge_configmaps(cms, GRANTS_DATA_KEY, |raw| Self::parse(raw).map(|g| g.map));
        (Self { map }, errors)
    }

    fn parse(yaml: &str) -> Result<Self, String> {
        let parsed: HashMap<String, Vec<BindingEntry>> =
            serde_yaml::from_str(yaml).map_err(|e| format!("parse bindings: {e}"))?;
        let map = parsed
            .into_iter()
            .map(|(agent, entries)| {
                let toolsets = entries
                    .into_iter()
                    .filter_map(|entry| match entry {
                        BindingEntry::Bare => None,
                        BindingEntry::Granted { grants, .. } if grants.is_empty() => None,
                        BindingEntry::Granted { name, grants } => {
                            Some((name, grants.into_keys().collect()))
                        }
                    })
                    .collect();
                (agent, toolsets)
            })
            .collect();
        Ok(Self { map })
    }

    /// The named agent's grants; an agent with no grant-bearing
    /// binding has an empty list.
    pub fn for_agent(&self, agent: &str) -> Vec<(String, Vec<String>)> {
        self.map.get(agent).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BINDINGS: &str = r#"
hello-world:
  - stdlib
  - name: hello-world
    grants:
      github:
        secret: demo-ssh-key
        egress: github.com
      deploy-key:
        secret: demo-ssh-key
        path: /home/agent/.ssh/id_ed25519
other-ag:
  - stdlib
"#;

    #[test]
    fn a_malformed_grants_block_fails_the_load() {
        let err = CapabilityGrants::parse(
            "hello-world:\n  - name: hello-world\n    grants: [github, deploy-key]\n",
        )
        .expect_err("a grants list is not a grant map");
        assert!(
            err.contains("expected a map"),
            "the error names the shape an operator must fix, got: {err}"
        );
    }

    #[test]
    fn for_agent_lists_sorted_grant_names_or_none_if_unknown() {
        let grants = CapabilityGrants::parse(BINDINGS).unwrap();
        assert_eq!(
            grants.for_agent("hello-world"),
            vec![(
                "hello-world".to_string(),
                vec!["deploy-key".to_string(), "github".to_string()],
            )],
            "bare entries carry no grants; grant names come back sorted"
        );
        assert!(grants.for_agent("nobody").is_empty());
    }

    /// The same bindings with every grant spec stripped: keys, empty values. This
    /// is the shape the chart mounts, so the relay must read it without ever
    /// seeing a Secret name, a mount path, or an egress domain.
    const NAMES_ONLY: &str = r#"
hello-world:
  - stdlib
  - name: hello-world
    grants:
      github: {}
      deploy-key: {}
other-ag:
  - stdlib
"#;

    // The relay keeps grant keys and discards every value, so a file carrying
    // no grant specs must yield the identical grants. This is what lets the chart
    // withhold the specs: a parser that started requiring `secret` would fail
    // here rather than in a running cluster.
    #[test]
    fn a_names_only_file_yields_the_same_grants_as_a_full_binding() {
        let full = CapabilityGrants::parse(BINDINGS).unwrap();
        let names_only = CapabilityGrants::parse(NAMES_ONLY).unwrap();
        assert_eq!(
            names_only.for_agent("hello-world"),
            full.for_agent("hello-world"),
            "the grant specs carry nothing the grant names need"
        );
        assert!(names_only.for_agent("other-ag").is_empty());
    }
}
