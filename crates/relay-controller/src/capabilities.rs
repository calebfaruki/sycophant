//! The per-workspace toolset grants, read once at startup from the
//! chart-rendered relay-toolset-grants file. Names only, and the file itself
//! carries nothing else: the relay is the internet-facing pod, so no grant's
//! Secret name, mount path, or egress domain is rendered into what it mounts. A
//! change to the grants rolls the pod through the chart's checksum annotation.

use std::collections::{BTreeMap, HashMap};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

/// One workspace's toolset list entry as the bindings file writes it: a bare
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

/// workspace → (toolset, grant names) pairs, in file order; grant names in
/// name order.
#[derive(Debug, Default, Clone)]
pub struct CapabilityGrants {
    map: HashMap<String, Vec<(String, Vec<String>)>>,
}

impl CapabilityGrants {
    pub fn load(path: &str) -> Result<Self, String> {
        let raw =
            std::fs::read_to_string(path).map_err(|e| format!("read bindings file {path}: {e}"))?;
        Self::parse(&raw)
    }

    fn parse(yaml: &str) -> Result<Self, String> {
        let parsed: HashMap<String, Vec<BindingEntry>> =
            serde_yaml::from_str(yaml).map_err(|e| format!("parse bindings: {e}"))?;
        let map = parsed
            .into_iter()
            .map(|(workspace, entries)| {
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
                (workspace, toolsets)
            })
            .collect();
        Ok(Self { map })
    }

    /// The named workspace's grants; a workspace with no grant-bearing
    /// binding has an empty list.
    pub fn for_workspace(&self, workspace: &str) -> Vec<(String, Vec<String>)> {
        self.map.get(workspace).cloned().unwrap_or_default()
    }

    /// Parse from a YAML literal, panicking on error. Test fixture only.
    #[cfg(test)]
    pub fn parse_for_tests(yaml: &str) -> Self {
        Self::parse(yaml).expect("test bindings must parse")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BINDINGS: &str = r#"
hello-world:
  - stdlib
  - name: ssh-credentials
    grants:
      github:
        secret: demo-ssh-key
        egress: github.com
      deploy-key:
        secret: demo-ssh-key
        path: /home/agent/.ssh/id_ed25519
other-ws:
  - stdlib
"#;

    #[test]
    fn a_malformed_grants_block_fails_the_load() {
        let err = CapabilityGrants::parse(
            "hello-world:\n  - name: ssh-credentials\n    grants: [github, deploy-key]\n",
        )
        .expect_err("a grants list is not a grant map");
        assert!(
            err.contains("expected a map"),
            "the error names the shape an operator must fix, got: {err}"
        );
    }

    #[test]
    fn a_grant_bearing_binding_lists_its_grant_names_per_toolset() {
        let grants = CapabilityGrants::parse(BINDINGS).unwrap();
        assert_eq!(
            grants.for_workspace("hello-world"),
            vec![(
                "ssh-credentials".to_string(),
                vec!["deploy-key".to_string(), "github".to_string()],
            )],
            "bare entries carry no grants; grant names come back sorted"
        );
    }

    #[test]
    fn a_workspace_with_only_bare_bindings_has_no_grants() {
        let grants = CapabilityGrants::parse(BINDINGS).unwrap();
        assert!(grants.for_workspace("other-ws").is_empty());
    }

    #[test]
    fn an_unknown_workspace_has_no_grants() {
        let grants = CapabilityGrants::parse(BINDINGS).unwrap();
        assert!(grants.for_workspace("nobody").is_empty());
    }

    /// The same bindings with every grant spec stripped: keys, empty values. This
    /// is the shape the chart mounts, so the relay must read it without ever
    /// seeing a Secret name, a mount path, or an egress domain.
    const NAMES_ONLY: &str = r#"
hello-world:
  - stdlib
  - name: ssh-credentials
    grants:
      github: {}
      deploy-key: {}
other-ws:
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
            names_only.for_workspace("hello-world"),
            full.for_workspace("hello-world"),
            "the grant specs carry nothing the grant names need"
        );
        assert!(names_only.for_workspace("other-ws").is_empty());
    }

    #[test]
    fn an_unparseable_bindings_file_is_an_error_not_an_empty_menu() {
        let err = CapabilityGrants::parse("{not yaml").unwrap_err();
        assert!(
            err.contains("parse bindings"),
            "the failure must name the parse, got: {err}"
        );
    }
}
