//! The capability manifest the harness reads once at boot from a mounted
//! ConfigMap. The chart renders it from the operator-authored toolset config and
//! this workspace's bindings, so the catalog needs no runtime discovery. It
//! carries each bound toolset's tools with their argument schemas and this
//! workspace's resolved grants (Secret name, mount path, egress). It stores no
//! Service address and no model section: the harness dials tools through the
//! per-workspace capability Service it already holds, and derives the inference
//! address from the model catalog.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use shared::toolset::{ArgDecl, CapabilityGrant};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapabilityManifest {
    #[serde(default)]
    pub tools: Vec<ManifestTool>,
}

/// One tool the workspace's bound toolsets expose, fully resolved: its
/// LLM-facing name and JSON `parameters`, the toolset that runs it, its argument
/// schema, and this workspace's grants for that toolset.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The tool's JSON Schema `parameters` object, carried as a JSON string so
    /// the harness forwards it to the model verbatim.
    pub parameters_json: String,
    pub toolset: String,
    #[serde(default)]
    pub args: Vec<ArgDecl>,
    #[serde(default)]
    pub grants: BTreeMap<String, CapabilityGrant>,
}

impl CapabilityManifest {
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "failed to read capability manifest file {}: {e}",
                path.display()
            )
        })?;
        serde_yaml::from_str(&content)
            .map_err(|e| format!("failed to parse capability manifest YAML: {e}"))
    }

    pub(crate) fn empty() -> Self {
        Self::default()
    }
}
