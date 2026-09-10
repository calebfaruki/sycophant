//! Toolset config types.
//!
//! An operator authors these as a chart-rendered ConfigMap the controller reads
//! once at startup. The shape is flat: each toolset entry carries everything
//! the controller acts on.

/// `ToolsetEntry` and its `Scalar` env value live in `shared::toolset`, mounted
/// the same way in the controller and the per-workspace harness. Re-exported
/// here so the controller's config surface keeps one path.
pub use shared::toolset::{Scalar, ToolsetEntry};

/// A Kubernetes Secret projected into a job by reference. The value is never
/// rendered as a string.
///
/// `file` is where the projected Secret lands: a read-only Secret-backed
/// volume at this path. Every credential is delivered as a file — environment
/// leaks through `/proc/<pid>/environ`, child process inheritance, and logs.
#[derive(Clone, Debug)]
pub struct SecretMapping {
    pub secret: String,
    pub file: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn parse_entry(yaml: &str) -> Result<ToolsetEntry, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
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
}
