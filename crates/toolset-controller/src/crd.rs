//! Toolset config types.
//!
//! An operator authors these as a chart-rendered ConfigMap the controller reads
//! once at startup. The shape is two-level: a toolset carries the two
//! attributes the controller acts on itself, plus a map of named profiles.

use std::collections::HashMap;

use serde::Deserialize;

/// One toolset entry.
///
/// `image` selects the worker pod; `keepalive` tells the controller when to reap
/// it. Neither is a profile key and neither is forwarded to the worker.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolsetEntry {
    #[serde(default)]
    pub image: Option<String>,

    #[serde(default)]
    pub keepalive: bool,

    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
}

/// One named profile within a toolset.
///
/// The controller reads `secrets` and `egress`. Every other key is inert: it is
/// forwarded into the worker as an environment variable, the key verbatim as
/// the name and its scalar as the value.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Profile {
    #[serde(default)]
    pub secrets: Vec<SecretMapping>,
    #[serde(default)]
    pub egress: Vec<EgressRule>,
    #[serde(flatten)]
    pub forwarded: HashMap<String, Scalar>,
}

/// An inert profile value. Only a scalar can become an environment variable, so
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

impl Profile {
    /// The inert profile keys as environment pairs, in a stable order.
    pub fn forwarded_env(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .forwarded
            .iter()
            .map(|(key, value)| (key.clone(), value.as_env_value()))
            .collect();
        out.sort();
        out
    }
}

/// A Kubernetes Secret projected into the worker by reference. The value is
/// never rendered as a string.
#[derive(Deserialize, Clone, Debug)]
#[serde(try_from = "RawSecretMapping")]
pub struct SecretMapping {
    pub secret: String,
    pub target: SecretTarget,
}

/// Where a projected Secret lands in the worker. The wire form sets exactly one
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

    fn parse_profile(yaml: &str) -> Result<Profile, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    #[test]
    fn secret_with_env_only_targets_an_env_var() {
        let profile = parse_profile("secrets:\n  - secret: github-token\n    env: GITHUB_TOKEN\n")
            .expect("an env-only secret parses");
        assert_eq!(profile.secrets.len(), 1);
        assert_eq!(profile.secrets[0].secret, "github-token");
        assert_eq!(
            profile.secrets[0].target,
            SecretTarget::Env("GITHUB_TOKEN".into())
        );
    }

    #[test]
    fn secret_with_file_only_targets_a_mounted_file() {
        let profile = parse_profile("secrets:\n  - secret: ssh-key\n    file: /run/secrets/id\n")
            .expect("a file-only secret parses");
        assert_eq!(
            profile.secrets[0].target,
            SecretTarget::File("/run/secrets/id".into())
        );
    }

    #[test]
    fn secret_setting_both_env_and_file_is_rejected() {
        let err = parse_profile(
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
        let err = parse_profile("secrets:\n  - secret: ssh-key\n")
            .expect_err("a secret with no target reaches the worker nowhere and must not parse");
        assert!(
            err.to_string().contains("ssh-key"),
            "the error names the offending secret, got: {err}"
        );
    }

    #[test]
    fn secret_with_an_unknown_field_is_rejected() {
        let err = parse_profile("secrets:\n  - secret: ssh-key\n    envv: SSH_KEY\n")
            .expect_err("a typo'd secret field must not be silently ignored");
        assert!(
            err.to_string().contains("envv"),
            "the error names the unknown field, got: {err}"
        );
    }

    /// The operator sees this error at the depth the controller actually parses:
    /// a whole ConfigMap, not a bare profile. With two profiles defining the same
    /// key, only the profile path tells them which one to fix.
    #[test]
    fn a_non_scalar_profile_value_is_rejected_and_the_error_names_the_profile() {
        let config = "notion:\n  image: ghcr.io/x/notion:1\n  profiles:\n    default:\n      TOOLSET_MODEL: gpt-4\n    writer:\n      TOOLSET_MODEL:\n        nested: map\n";
        let err = serde_yaml::from_str::<HashMap<String, ToolsetEntry>>(config)
            .expect_err("a non-scalar cannot become an environment variable");
        assert!(
            err.to_string().contains("notion.profiles.writer"),
            "the error must name the offending profile, not a sibling, got: {err}"
        );
    }

    #[test]
    fn scalar_profile_values_forward_in_stable_order() {
        let profile =
            parse_profile("TOOLSET_RETRIES: 3\nTOOLSET_MODEL: gpt-4\nTOOLSET_STREAM: true\n")
                .expect("scalars parse");
        assert_eq!(
            profile.forwarded_env(),
            vec![
                ("TOOLSET_MODEL".to_string(), "gpt-4".to_string()),
                ("TOOLSET_RETRIES".to_string(), "3".to_string()),
                ("TOOLSET_STREAM".to_string(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn secrets_and_egress_are_governed_and_never_forwarded_as_env() {
        let profile = parse_profile(
            "secrets:\n  - secret: k\n    env: K\negress:\n  - domain: notion.com\n    port: 443\nTOOLSET_MODEL: gpt-4\n",
        )
        .expect("a full profile parses");
        assert_eq!(
            profile.forwarded_env(),
            vec![("TOOLSET_MODEL".to_string(), "gpt-4".to_string())],
            "only inert keys forward; secrets and egress are read by the controller"
        );
    }
}
