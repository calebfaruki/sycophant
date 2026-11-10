//! Byte-substring scrubber for known secret values.
//!
//! Each component that holds a secret (toolset-runtime for toolset
//! credentials, inference-runtime for LLM provider keys) builds a
//! `ScrubSet` from a JSON registry of secrets read from a named env
//! var. The set replaces every literal occurrence of the secret value
//! (plus base64- and url-encoded variants) with `[REDACTED:<name>]` in
//! any string passing through `apply`. The replacement happens on every
//! outbound channel from the holder: toolset tool output, gRPC chunks,
//! and tracing log lines.

use base64::Engine;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ScrubEntry {
    pub name: String,
    pub env: Option<String>,
    pub file: Option<String>,
}

pub struct ScrubSet {
    replacements: Vec<(String, String)>,
}

impl ScrubSet {
    /// Build a `ScrubSet` from a JSON registry in the named env var.
    /// The JSON is a list of `{name, env|file}` entries; the actual
    /// secret value is loaded from the referenced env var or file.
    /// An empty or missing env var produces an empty (no-op) set.
    pub fn from_env_var(env_var_name: &str) -> Self {
        let empty = Self {
            replacements: vec![],
        };
        let Ok(json) = std::env::var(env_var_name) else {
            return empty;
        };
        if json.is_empty() {
            return empty;
        }
        let Ok(entries): Result<Vec<ScrubEntry>, _> = serde_json::from_str(&json) else {
            return empty;
        };

        let mut secrets = Vec::new();

        for entry in &entries {
            let value = if let Some(ref env_name) = entry.env {
                std::env::var(env_name).unwrap_or_default()
            } else if let Some(ref file_path) = entry.file {
                std::fs::read_to_string(file_path).unwrap_or_default()
            } else {
                continue;
            };
            secrets.push((entry.name.as_str(), value));
        }

        let secrets: Vec<(&str, &str)> = secrets
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        Self::from_secrets(&secrets)
    }

    /// Build a `ScrubSet` from `(name, value)` pairs already in hand. Each
    /// value is trimmed; an empty value is skipped.
    pub fn from_secrets(secrets: &[(&str, &str)]) -> Self {
        let mut replacements = Vec::new();

        for (name, value) in secrets {
            let value = value.trim().to_string();
            if value.is_empty() {
                continue;
            }

            let tag = format!("[REDACTED:{name}]");

            replacements.push((value.clone(), tag.clone()));

            let b64 = base64::engine::general_purpose::STANDARD.encode(&value);
            if b64 != value {
                replacements.push((b64, tag.clone()));
            }

            let url = urlencoding::encode(&value).into_owned();
            if url != value {
                replacements.push((url, tag));
            }
        }

        replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

        Self { replacements }
    }

    pub fn apply(&self, input: &str) -> String {
        let mut result = input.to_string();
        for (pattern, replacement) in &self.replacements {
            result = result.replace(pattern, replacement);
        }
        result
    }

    pub fn is_empty(&self) -> bool {
        self.replacements.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::io::Write;

    const ENV: &str = "TEST_SCRUB_REGISTRY";

    fn with_env<F: FnOnce()>(key: &str, val: &str, f: F) {
        std::env::set_var(key, val);
        f();
        std::env::remove_var(key);
    }

    fn with_scrub_env<F: FnOnce()>(json: &str, f: F) {
        with_env(ENV, json, f);
    }

    #[test]
    #[serial]
    fn empty_env_returns_empty_set() {
        std::env::remove_var(ENV);
        let set = ScrubSet::from_env_var(ENV);
        assert_eq!(set.apply("hello secret world"), "hello secret world");
        assert!(set.is_empty());
    }

    #[test]
    #[serial]
    fn an_env_secret_arms_the_scrub_set_and_redacts_its_value() {
        with_env("TEST_SECRET_1", "s3cret-value", || {
            with_scrub_env(r#"[{"name":"my-secret","env":"TEST_SECRET_1"}]"#, || {
                let set = ScrubSet::from_env_var(ENV);
                assert!(!set.is_empty());
                assert_eq!(
                    set.apply("output contains s3cret-value here"),
                    "output contains [REDACTED:my-secret] here"
                );
            });
        });
    }

    #[test]
    #[serial]
    fn single_file_secret_redacted() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "file-secret-val").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        with_scrub_env(
            &format!(r#"[{{"name":"file-cred","file":"{path}"}}]"#),
            || {
                let set = ScrubSet::from_env_var(ENV);
                assert_eq!(
                    set.apply("got file-secret-val from disk"),
                    "got [REDACTED:file-cred] from disk"
                );
            },
        );
    }

    #[test]
    #[serial]
    fn base64_encoded_value_redacted() {
        with_env("TEST_SECRET_B64", "my-api-key", || {
            with_scrub_env(r#"[{"name":"api-key","env":"TEST_SECRET_B64"}]"#, || {
                let set = ScrubSet::from_env_var(ENV);
                let b64 = base64::engine::general_purpose::STANDARD.encode("my-api-key");
                assert_eq!(
                    set.apply(&format!("encoded: {b64}")),
                    "encoded: [REDACTED:api-key]"
                );
            });
        });
    }

    #[test]
    #[serial]
    fn url_encoded_value_redacted() {
        with_env("TEST_SECRET_URL", "key=val&foo=bar", || {
            with_scrub_env(r#"[{"name":"url-cred","env":"TEST_SECRET_URL"}]"#, || {
                let set = ScrubSet::from_env_var(ENV);
                let encoded = urlencoding::encode("key=val&foo=bar");
                assert_eq!(
                    set.apply(&format!("url: {encoded}")),
                    "url: [REDACTED:url-cred]"
                );
            });
        });
    }

    #[test]
    #[serial]
    fn multiple_secrets_all_redacted() {
        with_env("TEST_SEC_A", "alpha", || {
            with_env("TEST_SEC_B", "bravo", || {
                with_scrub_env(
                    r#"[{"name":"a","env":"TEST_SEC_A"},{"name":"b","env":"TEST_SEC_B"}]"#,
                    || {
                        let set = ScrubSet::from_env_var(ENV);
                        assert_eq!(
                            set.apply("alpha and bravo"),
                            "[REDACTED:a] and [REDACTED:b]"
                        );
                    },
                );
            });
        });
    }

    #[test]
    #[serial]
    fn partial_match_not_redacted() {
        with_env("TEST_SECRET_FULL", "fullmatch", || {
            with_scrub_env(r#"[{"name":"full","env":"TEST_SECRET_FULL"}]"#, || {
                let set = ScrubSet::from_env_var(ENV);
                assert_eq!(set.apply("fullmatch"), "[REDACTED:full]");
                assert_eq!(set.apply("no match here"), "no match here");
            });
        });
    }

    #[test]
    #[serial]
    fn empty_secret_value_skipped() {
        with_env("TEST_EMPTY", "", || {
            with_scrub_env(r#"[{"name":"empty","env":"TEST_EMPTY"}]"#, || {
                let set = ScrubSet::from_env_var(ENV);
                assert!(set.is_empty());
            });
        });
    }

    #[test]
    #[serial]
    fn longest_match_first() {
        with_env("TEST_SHORT", "abc", || {
            with_env("TEST_LONG", "abcdef", || {
                with_scrub_env(
                    r#"[{"name":"short","env":"TEST_SHORT"},{"name":"long","env":"TEST_LONG"}]"#,
                    || {
                        let set = ScrubSet::from_env_var(ENV);
                        assert_eq!(set.apply("abcdef"), "[REDACTED:long]");
                    },
                );
            });
        });
    }

    #[test]
    #[serial]
    fn distinct_env_var_names_are_independent() {
        with_env("TEST_SECRET_X", "value-x", || {
            with_env(
                "TOOLSET_SCRUB_SECRETS",
                r#"[{"name":"x","env":"TEST_SECRET_X"}]"#,
                || {
                    let set = ScrubSet::from_env_var("__UNSET_SCRUB_INDEPENDENCE_TEST__");
                    assert!(
                        set.is_empty(),
                        "an unset var must not pick up TOOLSET_SCRUB_SECRETS"
                    );
                },
            );
        });
    }
}
