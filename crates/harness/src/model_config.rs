//! Model resolution for the harness.
//!
//! The harness owns model selection. It resolves the value an agent
//! definition's `model:` names to a `ModelConfig`, then dials the model
//! itself. A local model is a warm in-cluster Service the harness dials
//! directly; a remote model is reached through a per-call inference-runtime job
//! the harness creates. An absent name is refused, never defaulted.
//!
//! The harness reads an operator-authored config from a chart-rendered
//! ConfigMap mounted read-only into the pod; a change rolls the harness.

use std::collections::HashMap;

use serde::Deserialize;

/// One model the harness can dial, keyed in `ModelConfigs` by the value an
/// agent definition's `model:` names. `format` is the provider wire format and
/// `model` the provider-side model id; the `kind` discriminator carries how the
/// harness reaches it.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum ModelConfig {
    /// A warm in-cluster model served by a chart-static Deployment behind the
    /// `inference-<key>` ClusterIP Service. The harness dials the Service
    /// directly; no per-call job is created.
    Local {
        format: String,
        model: String,
        /// The Service's serving port, parsed by the chart from the profile
        /// `baseUrl`.
        port: u16,
    },
    /// A provider reached over the public internet through a per-call
    /// inference-runtime job the harness creates. The job holds the provider
    /// credential and the external egress; the harness dials the job.
    #[serde(rename_all = "camelCase")]
    Remote {
        format: String,
        model: String,
        base_url: String,
        /// The inference-runtime job image the harness stamps into the Job.
        image: String,
        /// Name of the Kubernetes Secret carrying the provider credential.
        /// Mounted by reference into the job; the value is never read here.
        /// Absent when the destination needs no credential.
        secret: Option<String>,
    },
}

impl ModelConfig {
    /// The provider wire format, common to both targets.
    pub(crate) fn format(&self) -> &str {
        match self {
            ModelConfig::Local { format, .. } | ModelConfig::Remote { format, .. } => format,
        }
    }

    /// The provider-side model id, common to both targets.
    pub(crate) fn model(&self) -> &str {
        match self {
            ModelConfig::Local { model, .. } | ModelConfig::Remote { model, .. } => model,
        }
    }
}

/// The harness's model catalog. Keyed by the value an agent definition's
/// `model:` names; an absent key is refused, never defaulted.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelConfigs {
    #[serde(default)]
    models: HashMap<String, ModelConfig>,
}

impl ModelConfigs {
    /// Read once at startup from the chart-rendered ConfigMap mounted into the
    /// harness pod; a change rolls the harness.
    pub(crate) fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read model config file {path}: {e}"))?;
        serde_yaml::from_str(&content)
            .map_err(|e| format!("failed to parse model config YAML: {e}"))
    }

    #[cfg(test)]
    pub(crate) fn from_map(models: HashMap<String, ModelConfig>) -> Self {
        Self { models }
    }

    /// The model the agent-definition value names. Absent is refused, never
    /// defaulted: there is no fallback model.
    pub(crate) fn get(&self, name: &str) -> Option<&ModelConfig> {
        self.models.get(name)
    }

    /// The configured model names, sorted, for diagnostics.
    pub(crate) fn names(&self) -> Vec<String> {
        let mut out: Vec<String> = self.models.keys().cloned().collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse at the depth the harness actually loads: the whole catalog, not a
    /// bare inner entry.
    fn parse(yaml: &str) -> Result<ModelConfigs, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    const CATALOG: &str = "\
models:
  in-cluster:
    kind: local
    format: openai
    model: liquid/lfm2.5-8b-a1b
    port: 8080
  deepseek-v4-flash:
    kind: remote
    format: openai
    model: deepseek/deepseek-v4-flash
    baseUrl: https://openrouter.ai/api/v1
    image: ghcr.io/sycophant/inference-runtime:1
    secret: sycophant-llm-openrouter
";

    /// A named local config resolves to a local target carrying the warm
    /// Service's serving port. The discriminator must survive the round trip:
    /// a mutant that maps a `local` entry to `Remote` (or defaults the port)
    /// fails the port assertion.
    #[test]
    fn a_named_local_config_resolves_to_a_local_target() {
        let catalog = parse(CATALOG).expect("the catalog parses");
        match catalog.get("in-cluster") {
            Some(ModelConfig::Local {
                format,
                model,
                port,
            }) => {
                assert_eq!(format, "openai");
                assert_eq!(model, "liquid/lfm2.5-8b-a1b");
                assert_eq!(*port, 8080);
            }
            other => panic!("a local entry must resolve to ModelConfig::Local, got {other:?}"),
        }
    }

    /// A named remote config resolves to a remote target carrying its provider
    /// base URL, job image, and the name of its credential secret.
    #[test]
    fn a_named_remote_config_resolves_to_a_remote_target_with_its_secret() {
        let catalog = parse(CATALOG).expect("the catalog parses");
        match catalog.get("deepseek-v4-flash") {
            Some(ModelConfig::Remote {
                format,
                model,
                base_url,
                image,
                secret,
            }) => {
                assert_eq!(format, "openai");
                assert_eq!(model, "deepseek/deepseek-v4-flash");
                assert_eq!(base_url, "https://openrouter.ai/api/v1");
                assert_eq!(image, "ghcr.io/sycophant/inference-runtime:1");
                assert_eq!(secret.as_deref(), Some("sycophant-llm-openrouter"));
            }
            other => panic!("a remote entry must resolve to ModelConfig::Remote, got {other:?}"),
        }
    }

    /// A remote destination inside the cluster authenticates nobody, so its
    /// config names no secret and still resolves.
    #[test]
    fn a_remote_config_resolves_when_it_names_no_secret() {
        let yaml = CATALOG.replace("    secret: sycophant-llm-openrouter\n", "");
        let catalog = parse(&yaml).expect("a remote config with no secret still parses");
        match catalog.get("deepseek-v4-flash") {
            Some(ModelConfig::Remote { secret, .. }) => assert_eq!(*secret, None),
            other => panic!("expected a remote target, got {other:?}"),
        }
    }

    /// An absent name resolves to `None` — never a default. A mutant that
    /// invents a fallback model fails this.
    #[test]
    fn an_absent_name_resolves_to_none() {
        let catalog = parse(CATALOG).expect("the catalog parses");
        assert_eq!(catalog.get("no-such-model"), None);
    }

    /// The empty catalog refuses every name; nothing is defaulted.
    #[test]
    fn the_empty_catalog_resolves_every_name_to_none() {
        let catalog = ModelConfigs::default();
        assert_eq!(catalog.get("in-cluster"), None);
    }

    #[test]
    fn names_returns_sorted_model_keys() {
        let catalog = parse(CATALOG).expect("the catalog parses");
        assert_eq!(
            catalog.names(),
            vec!["deepseek-v4-flash".to_string(), "in-cluster".to_string()]
        );
    }

    /// `format`/`model` read the shared fields regardless of target, so a
    /// caller need not destructure the discriminator to compose the provider
    /// call.
    #[test]
    fn format_and_model_read_the_shared_fields_on_both_targets() {
        let catalog = parse(CATALOG).expect("the catalog parses");
        let local = catalog.get("in-cluster").expect("local present");
        assert_eq!(local.format(), "openai");
        assert_eq!(local.model(), "liquid/lfm2.5-8b-a1b");
        let remote = catalog.get("deepseek-v4-flash").expect("remote present");
        assert_eq!(remote.format(), "openai");
        assert_eq!(remote.model(), "deepseek/deepseek-v4-flash");
    }

    /// `from_map` builds a catalog the resolver reads the same as a parsed one.
    #[test]
    fn from_map_builds_a_resolvable_catalog() {
        let mut models = HashMap::new();
        models.insert(
            "in-cluster".to_string(),
            ModelConfig::Local {
                format: "openai".to_string(),
                model: "liquid/lfm2.5-8b-a1b".to_string(),
                port: 8080,
            },
        );
        let catalog = ModelConfigs::from_map(models);
        assert!(matches!(
            catalog.get("in-cluster"),
            Some(ModelConfig::Local { port: 8080, .. })
        ));
    }

    /// `load` reads and parses the mounted file, the harness's startup path.
    #[test]
    fn load_reads_and_parses_the_mounted_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("model.yaml");
        std::fs::write(&path, CATALOG).unwrap();
        let catalog = ModelConfigs::load(path.to_str().unwrap()).expect("the mounted file loads");
        assert!(matches!(
            catalog.get("deepseek-v4-flash"),
            Some(ModelConfig::Remote { .. })
        ));
    }
}
