//! Toolset config types.
//!
//! An operator authors these as a chart-rendered ConfigMap the controller reads
//! once at startup. The shape is flat: each toolset entry carries everything
//! the controller acts on.

use std::collections::HashMap;

use serde::Deserialize;

/// One toolset entry.
///
/// `image` selects the tool job's pod; `keepalive` tells the controller when to
/// reap it. Neither is forwarded to the tool job. `secrets` projects Kubernetes
/// Secrets by reference, `egress` is read by the chart, and `env` forwards
/// each key verbatim into the tool job as an environment variable.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolsetEntry {
    #[serde(default)]
    pub image: Option<String>,

    #[serde(default)]
    pub keepalive: bool,

    #[serde(default)]
    pub secrets: Vec<SecretMapping>,

    #[serde(default)]
    pub egress: Vec<EgressRule>,

    #[serde(default)]
    pub env: HashMap<String, Scalar>,
}

/// An `env` value. Only a scalar can become an environment variable, so
/// the type admits nothing else and a map or list fails the parse.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Scalar {
    Bool(bool),
    Number(serde_yaml::Number),
    String(String),
}

impl Scalar {
    fn as_env_value(&self) -> String {
        match self {
            Scalar::Bool(b) => b.to_string(),
            Scalar::Number(n) => n.to_string(),
            Scalar::String(s) => s.clone(),
        }
    }
}

impl ToolsetEntry {
    /// The `env` keys as environment pairs, in a stable order.
    pub fn forwarded_env(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.as_env_value()))
            .collect();
        out.sort();
        out
    }
}

/// A Kubernetes Secret projected into the tool job by reference. The value is
/// never rendered as a string.
#[derive(Deserialize, Clone, Debug)]
#[serde(try_from = "RawSecretMapping")]
pub struct SecretMapping {
    pub secret: String,
    pub target: SecretTarget,
}

/// Where a projected Secret lands in the tool job. The wire form sets exactly one
/// of `env` or `file`; this type holds the choice so no consumer re-tests it.
#[derive(Clone, Debug, PartialEq)]
pub enum SecretTarget {
    /// A `secretKeyRef` environment variable of this name.
    Env(String),
    /// A read-only Secret-backed volume at this path.
    File(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecretMapping {
    secret: String,
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    file: Option<String>,
}

impl TryFrom<RawSecretMapping> for SecretMapping {
    type Error = String;

    fn try_from(raw: RawSecretMapping) -> Result<Self, Self::Error> {
        let target = match (raw.env, raw.file) {
            (Some(env), None) => SecretTarget::Env(env),
            (None, Some(file)) => SecretTarget::File(file),
            _ => {
                return Err(format!(
                    "secret '{}' must set exactly one of `env` or `file`",
                    raw.secret
                ))
            }
        };
        Ok(SecretMapping {
            secret: raw.secret,
            target,
        })
    }
}

/// The prompt configuration section. The prompt toolset is the hardcoded turn
/// server, so it is not an entry of the toolsets map and appears in no
/// workspace's toolset bindings: the controller reads this section directly.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct PromptConfig {
    /// Keyed by the turn's `model` value. An absent key is refused, never
    /// defaulted.
    #[serde(default)]
    pub profiles: HashMap<String, PromptProfile>,
}

/// Read once at startup from the same chart-rendered ConfigMap as the toolset
/// config; a change rolls the controller.
impl PromptConfig {
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read prompt config file {path}: {e}"))?;
        serde_yaml::from_str(&content)
            .map_err(|e| format!("failed to parse prompt config YAML: {e}"))
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_map(profiles: HashMap<String, PromptProfile>) -> Self {
        Self { profiles }
    }

    /// The profile a turn's `model` value names. Absent is refused, never
    /// defaulted.
    pub fn get(&self, profile_key: &str) -> Option<&PromptProfile> {
        self.profiles.get(profile_key)
    }

    pub fn names(&self) -> Vec<String> {
        let mut out: Vec<String> = self.profiles.keys().cloned().collect();
        out.sort();
        out
    }
}

/// One prompt profile. Every key is required except `egress`: the prompt image
/// fails closed without a format, a model, and a base URL, and the provider
/// secret is what lets it reach the provider at all. `egress` is read by the
/// chart, which renders the profile's provider CiliumNetworkPolicy.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PromptProfile {
    pub image: String,
    pub format: String,
    pub model: String,
    pub base_url: String,
    /// Name of the Kubernetes Secret carrying the provider credential. The
    /// controller mounts it by reference and never reads its value.
    pub secret: String,
    #[serde(default)]
    pub egress: Vec<EgressRule>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct EgressRule {
    /// Trust principal for the egress: the registrable domain (e.g.
    /// `notion.com`, `github.com`) whose zone the toolset is allowed
    /// to reach. The chart trusts the apex AND its single-label
    /// subdomain space (`*.notion.com`), so a `domain: notion.com`
    /// declaration covers `api.notion.com`, `auth.notion.com`, etc.
    /// The same principal controls the whole subtree; enumerating
    /// individual subdomains adds friction without security gain.
    pub domain: String,
    pub port: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_entry(yaml: &str) -> Result<ToolsetEntry, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    #[test]
    fn secret_with_env_only_targets_an_env_var() {
        let entry = parse_entry("secrets:\n  - secret: github-token\n    env: GITHUB_TOKEN\n")
            .expect("an env-only secret parses");
        assert_eq!(entry.secrets.len(), 1);
        assert_eq!(entry.secrets[0].secret, "github-token");
        assert_eq!(
            entry.secrets[0].target,
            SecretTarget::Env("GITHUB_TOKEN".into())
        );
    }

    #[test]
    fn secret_with_file_only_targets_a_mounted_file() {
        let entry = parse_entry("secrets:\n  - secret: ssh-key\n    file: /run/secrets/id\n")
            .expect("a file-only secret parses");
        assert_eq!(
            entry.secrets[0].target,
            SecretTarget::File("/run/secrets/id".into())
        );
    }

    #[test]
    fn secret_setting_both_env_and_file_is_rejected() {
        let err = parse_entry(
            "secrets:\n  - secret: ssh-key\n    env: SSH_KEY\n    file: /run/secrets/id\n",
        )
        .expect_err("a secret naming both targets is ambiguous and must not parse");
        assert!(
            err.to_string().contains("exactly one"),
            "the error names the constraint, got: {err}"
        );
    }

    #[test]
    fn secret_setting_neither_env_nor_file_is_rejected() {
        let err = parse_entry("secrets:\n  - secret: ssh-key\n")
            .expect_err("a secret with no target reaches the tool job nowhere and must not parse");
        assert!(
            err.to_string().contains("ssh-key"),
            "the error names the offending secret, got: {err}"
        );
    }

    #[test]
    fn secret_with_an_unknown_field_is_rejected() {
        let err = parse_entry("secrets:\n  - secret: ssh-key\n    envv: SSH_KEY\n")
            .expect_err("a typo'd secret field must not be silently ignored");
        assert!(
            err.to_string().contains("envv"),
            "the error names the unknown field, got: {err}"
        );
    }

    /// The operator sees this error at the depth the controller actually parses:
    /// a whole ConfigMap, not a bare entry. With two toolsets defining the same
    /// `env` key, only the key path tells them which one to fix.
    #[test]
    fn a_non_scalar_env_value_is_rejected_and_the_error_names_the_key_path() {
        let config = "stdlib:\n  image: ghcr.io/x/stdlib:1\n  env:\n    TOOLSET_MODEL: gpt-4\nnotion:\n  image: ghcr.io/x/notion:1\n  env:\n    TOOLSET_MODEL:\n      nested: map\n";
        let err = serde_yaml::from_str::<HashMap<String, ToolsetEntry>>(config)
            .expect_err("a non-scalar cannot become an environment variable");
        assert!(
            err.to_string().contains("notion.env"),
            "the error must name the offending toolset's env, not a sibling, got: {err}"
        );
    }

    /// The flatten is gone, so `deny_unknown_fields` holds again: a stray key on
    /// an entry is a typo, never a silently-dropped env var.
    #[test]
    fn an_unknown_entry_key_is_rejected() {
        let err = parse_entry("image: ghcr.io/x/notion:1\nNOTION_API_VERSION: \"2022-06-28\"\n")
            .expect_err("a top-level key outside the schema must not parse");
        assert!(
            err.to_string().contains("NOTION_API_VERSION"),
            "the error names the unknown key, got: {err}"
        );
    }

    // ---- Prompt configuration ----

    /// Parse at the depth the controller actually loads: the whole prompt
    /// section, not a bare inner struct.
    fn parse_prompt_config(yaml: &str) -> Result<PromptConfig, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    const FULL_PROMPT_SECTION: &str = "\
profiles:
  deepseek-v4-flash:
    image: ghcr.io/sycophant/prompt-toolset:1
    format: openai
    model: deepseek/deepseek-v4-flash
    baseUrl: https://openrouter.ai/api/v1
    secret: sycophant-llm-openrouter
    egress:
      - domain: openrouter.ai
        port: 443
";

    #[test]
    fn prompt_profile_parses_image_format_model_base_url_secret_and_egress() {
        let config = parse_prompt_config(FULL_PROMPT_SECTION).expect("the prompt section parses");
        let profile = config
            .profiles
            .get("deepseek-v4-flash")
            .expect("the profile is keyed by the turn's model value");
        assert_eq!(profile.image, "ghcr.io/sycophant/prompt-toolset:1");
        assert_eq!(profile.format, "openai");
        assert_eq!(profile.model, "deepseek/deepseek-v4-flash");
        assert_eq!(profile.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(profile.secret, "sycophant-llm-openrouter");
        assert_eq!(profile.egress.len(), 1);
        assert_eq!(profile.egress[0].domain, "openrouter.ai");
        assert_eq!(profile.egress[0].port, 443);
    }

    #[test]
    fn prompt_profile_rejects_an_unknown_key() {
        let yaml = FULL_PROMPT_SECTION.replace("    secret:", "    secrets:");
        let err = parse_prompt_config(&yaml)
            .expect_err("a typo'd prompt key must not be silently ignored");
        assert!(
            err.to_string().contains("secrets"),
            "the error names the unknown key, got: {err}"
        );
    }

    #[test]
    fn prompt_profile_rejects_a_missing_required_key() {
        let yaml = FULL_PROMPT_SECTION.replace("    model: deepseek/deepseek-v4-flash\n", "");
        let err = parse_prompt_config(&yaml)
            .expect_err("a prompt profile with no model fails the image closed and must not parse");
        assert!(
            err.to_string().contains("model"),
            "the error names the missing key, got: {err}"
        );
    }

    #[test]
    fn scalar_env_values_forward_in_stable_order() {
        let entry = parse_entry(
            "env:\n  TOOLSET_RETRIES: 3\n  TOOLSET_MODEL: gpt-4\n  TOOLSET_STREAM: true\n",
        )
        .expect("scalars parse");
        assert_eq!(
            entry.forwarded_env(),
            vec![
                ("TOOLSET_MODEL".to_string(), "gpt-4".to_string()),
                ("TOOLSET_RETRIES".to_string(), "3".to_string()),
                ("TOOLSET_STREAM".to_string(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn secrets_and_egress_are_governed_and_never_forwarded_as_env() {
        let entry = parse_entry(
            "secrets:\n  - secret: k\n    env: K\negress:\n  - domain: notion.com\n    port: 443\nenv:\n  TOOLSET_MODEL: gpt-4\n",
        )
        .expect("a full entry parses");
        assert_eq!(
            entry.forwarded_env(),
            vec![("TOOLSET_MODEL".to_string(), "gpt-4".to_string())],
            "only `env` keys forward; secrets and egress are read by the controller"
        );
    }
}
