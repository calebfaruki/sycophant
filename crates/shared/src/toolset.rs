//! Toolset config and tool-call validation: the types behind the
//! chart-rendered `toolset-config` ConfigMap the harness reads at boot.
//! `CapabilityGrant` pairs one Secret with one egress destination,
//! `ToolsetConfig`/`ToolsetEntry` carry each toolset's runtime shape, and
//! `validate_call_input` checks an LLM's arguments against a tool's `ArgDecl`s.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

/// Conventional mount path for the workspace PVC inside every tool Job.
/// Not configurable: tool images target `/workspace`.
pub const WORKSPACE_MOUNT_PATH: &str = "/workspace";

/// The projected ServiceAccount token mount every tool job depends on.
const SA_TOKEN_MOUNT_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// The image's dispatch entrypoint directory.
const DISPATCH_MOUNT_PATH: &str = "/etc/toolset";

/// One operator-approved credential, scoped to one (workspace, toolset) pair.
///
/// `secret` names the Kubernetes Secret carrying it. `path` is where the
/// credential file lands. `egress` names the one domain the chart opens for it;
/// a grant declaring none mounts its secret and opens nothing.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawGrant")]
pub struct CapabilityGrant {
    pub secret: String,
    pub path: Option<String>,
    pub egress: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrant {
    secret: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    egress: Option<String>,
}

impl TryFrom<RawGrant> for CapabilityGrant {
    type Error = String;

    fn try_from(raw: RawGrant) -> Result<Self, Self::Error> {
        if raw.secret.is_empty() {
            return Err("a grant names exactly one Secret, so `secret` must not be empty".into());
        }
        if let Some(path) = &raw.path {
            if !path.starts_with('/') {
                return Err(format!(
                    "grant `path` must be an absolute mount target, got {path:?}"
                ));
            }
            let reserved = path == SA_TOKEN_MOUNT_PATH
                || path == DISPATCH_MOUNT_PATH
                || path.starts_with(&format!("{DISPATCH_MOUNT_PATH}/"))
                || path == WORKSPACE_MOUNT_PATH;
            if reserved {
                return Err(format!(
                    "grant `path` {path} is a reserved mount the tool job already depends on"
                ));
            }
        }
        Ok(CapabilityGrant {
            secret: raw.secret,
            path: raw.path,
            egress: raw.egress,
        })
    }
}

/// The operator-authored toolset config, read once at startup from a
/// chart-rendered ConfigMap. There is no watch: a config change rolls the
/// reader's pod.
#[derive(Clone, Default)]
pub struct ToolsetConfig {
    map: HashMap<String, ToolsetEntry>,
}

impl ToolsetConfig {
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read toolset config file {path}: {e}"))?;
        let map: HashMap<String, ToolsetEntry> = serde_yaml::from_str(&content)
            .map_err(|e| format!("failed to parse toolset config YAML: {e}"))?;
        Ok(Self { map })
    }

    pub fn empty() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn from_map(map: HashMap<String, ToolsetEntry>) -> Self {
        Self { map }
    }

    pub fn get(&self, name: &str) -> Option<&ToolsetEntry> {
        self.map.get(name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut out: Vec<String> = self.map.keys().cloned().collect();
        out.sort();
        out
    }

    pub fn entries(&self) -> impl Iterator<Item = (&String, &ToolsetEntry)> {
        self.map.iter()
    }
}

/// One toolset entry. Runtime shape only: it owns no credential and no network
/// hole.
///
/// `image` selects the tool job's pod; `keepalive` tells the reader when to reap
/// it. Neither is forwarded to the tool job. `env` forwards each key verbatim
/// into the tool job as an environment variable.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolsetEntry {
    #[serde(default)]
    pub image: Option<String>,

    #[serde(default)]
    pub keepalive: bool,

    #[serde(default, rename = "deadlineSeconds")]
    pub deadline_seconds: Option<u64>,

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

/// Convert an LLM-facing tool name to a K8s name segment (RFC 1123:
/// `[a-z0-9]([-a-z0-9]*[a-z0-9])?`). Used to build tool Job names from
/// PascalCase / camelCase / snake_case canonical identifiers.
///
/// Rules:
/// - `_` → `-`
/// - Uppercase becomes lowercase; a `-` is inserted before it when the
///   previous character is lowercase or a digit (camelCase boundary), or
///   when the previous character is uppercase and the next is lowercase
///   (acronym-to-Title boundary, e.g. `XMLHttp` → `xml-http`).
/// - Leading/trailing hyphens are trimmed to satisfy RFC 1123.
pub fn tool_name_to_k8s_segment(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = String::with_capacity(bytes.len() + 4);
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'_' {
            if !out.ends_with('-') {
                out.push('-');
            }
            continue;
        }
        if b.is_ascii_uppercase() {
            let prev_lower_or_digit =
                i > 0 && (bytes[i - 1].is_ascii_lowercase() || bytes[i - 1].is_ascii_digit());
            let prev_upper = i > 0 && bytes[i - 1].is_ascii_uppercase();
            let next_lower = bytes
                .get(i + 1)
                .map(|c| c.is_ascii_lowercase())
                .unwrap_or(false);
            if !out.ends_with('-') && (prev_lower_or_digit || (prev_upper && next_lower)) {
                out.push('-');
            }
            out.push((b as char).to_ascii_lowercase());
        } else {
            out.push(b as char);
        }
    }
    out.trim_matches('-').to_string()
}

/// A single declared tool argument: its LLM-facing `name`, JSON `ty`, whether
/// it is `required`, the `env` var the runtime sets from its value, and an
/// optional `description`. Feeds the per-workspace harness dispatch producer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgDecl {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ArgType,
    #[serde(default)]
    pub required: bool,
    pub env: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgType {
    String,
    Integer,
    Number,
    Boolean,
}

impl ArgType {
    pub fn as_schema_str(&self) -> &'static str {
        match self {
            ArgType::String => "string",
            ArgType::Integer => "integer",
            ArgType::Number => "number",
            ArgType::Boolean => "boolean",
        }
    }

    pub fn from_schema_str(s: &str) -> Option<ArgType> {
        match s {
            "string" => Some(ArgType::String),
            "integer" => Some(ArgType::Integer),
            "number" => Some(ArgType::Number),
            "boolean" => Some(ArgType::Boolean),
            _ => None,
        }
    }
}

/// The terminal reasons an LLM-provided `input_json` fails validation against a
/// tool's declared `args`. Each maps to a gRPC `InvalidArgument` at the
/// dispatch boundary via `From<ArgValidationError> for tonic::Status`.
#[derive(Debug, thiserror::Error)]
pub enum ArgValidationError {
    #[error("input is not valid JSON: {0}")]
    NotJson(String),
    #[error("input must be a JSON object")]
    NotObject,
    #[error("unknown input field '{field}' (declared: {declared})")]
    UnknownField { field: String, declared: String },
    #[error("missing required input field '{0}'")]
    MissingRequired(String),
    #[error("input field '{field}' has wrong type (expected {expected})")]
    WrongType { field: String, expected: String },
}

impl From<ArgValidationError> for tonic::Status {
    fn from(e: ArgValidationError) -> Self {
        tonic::Status::invalid_argument(e.to_string())
    }
}

/// Validate an LLM-provided `input_json` payload against a tool's declared
/// `args`. On success, returns a map of `env_var_name -> value_as_string`
/// — the env vars the runtime will set on the `make` invocation. The map
/// only contains entries the LLM provided (optional+missing args are absent
/// from the result; they do not appear as empty env vars).
pub fn validate_call_input(
    input_json: &str,
    args: &[ArgDecl],
) -> Result<HashMap<String, String>, ArgValidationError> {
    let parsed: serde_json::Value =
        serde_json::from_str(input_json).map_err(|e| ArgValidationError::NotJson(e.to_string()))?;

    let obj = parsed.as_object().ok_or(ArgValidationError::NotObject)?;

    let declared: HashSet<&str> = args.iter().map(|a| a.name.as_str()).collect();
    for key in obj.keys() {
        if !declared.contains(key.as_str()) {
            return Err(ArgValidationError::UnknownField {
                field: key.clone(),
                declared: format!("{declared:?}"),
            });
        }
    }

    let mut env_map = HashMap::new();
    for arg in args {
        let Some(value) = obj.get(&arg.name) else {
            if arg.required {
                return Err(ArgValidationError::MissingRequired(arg.name.clone()));
            }
            continue;
        };

        let str_val = match (&arg.ty, value) {
            (ArgType::String, serde_json::Value::String(s)) => s.clone(),
            (ArgType::Integer, serde_json::Value::Number(n)) if n.is_i64() || n.is_u64() => {
                n.to_string()
            }
            (ArgType::Number, serde_json::Value::Number(n)) => n.to_string(),
            (ArgType::Boolean, serde_json::Value::Bool(b)) => b.to_string(),
            _ => {
                return Err(ArgValidationError::WrongType {
                    field: arg.name.clone(),
                    expected: arg.ty.as_schema_str().to_string(),
                });
            }
        };
        env_map.insert(arg.env.clone(), str_val);
    }

    Ok(env_map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arg(name: &str, ty: ArgType, required: bool, env: &str) -> ArgDecl {
        ArgDecl {
            name: name.to_string(),
            ty,
            required,
            env: env.to_string(),
            description: None,
        }
    }

    #[test]
    fn validate_required_string_ok() {
        let args = vec![arg("query", ArgType::String, true, "QUERY")];
        let env = validate_call_input(r#"{"query": "hello"}"#, &args).unwrap();
        assert_eq!(env.get("QUERY"), Some(&"hello".to_string()));
    }

    #[test]
    fn validate_optional_missing_skipped() {
        let args = vec![
            arg("query", ArgType::String, true, "QUERY"),
            arg("filter", ArgType::String, false, "FILTER"),
        ];
        let env = validate_call_input(r#"{"query": "x"}"#, &args).unwrap();
        assert_eq!(env.len(), 1);
        assert!(!env.contains_key("FILTER"));
    }

    #[test]
    fn validate_optional_present_included() {
        let args = vec![arg("filter", ArgType::String, false, "FILTER")];
        let env = validate_call_input(r#"{"filter": "f"}"#, &args).unwrap();
        assert_eq!(env.get("FILTER"), Some(&"f".to_string()));
    }

    #[test]
    fn validate_missing_required_errors() {
        let args = vec![arg("query", ArgType::String, true, "QUERY")];
        let err = validate_call_input(r#"{}"#, &args).unwrap_err();
        assert!(err
            .to_string()
            .contains("missing required input field 'query'"));
    }

    #[test]
    fn validate_unknown_field_errors() {
        let args = vec![arg("query", ArgType::String, true, "QUERY")];
        let err = validate_call_input(r#"{"query": "x", "bogus": "y"}"#, &args).unwrap_err();
        assert!(err.to_string().contains("unknown input field 'bogus'"));
    }

    #[test]
    fn validate_wrong_type_string_for_integer_errors() {
        let args = vec![arg("n", ArgType::Integer, true, "N")];
        let err = validate_call_input(r#"{"n": "not a number"}"#, &args).unwrap_err();
        assert!(err.to_string().contains("wrong type"));
    }

    #[test]
    fn validate_integer_rejects_float() {
        let args = vec![arg("n", ArgType::Integer, true, "N")];
        assert!(validate_call_input(r#"{"n": 1.5}"#, &args).is_err());
    }

    #[test]
    fn validate_integer_accepts_positive() {
        let args = vec![arg("n", ArgType::Integer, true, "N")];
        let env = validate_call_input(r#"{"n": 42}"#, &args).unwrap();
        assert_eq!(env.get("N"), Some(&"42".to_string()));
    }

    #[test]
    fn validate_integer_accepts_negative() {
        let args = vec![arg("n", ArgType::Integer, true, "N")];
        let env = validate_call_input(r#"{"n": -1}"#, &args).unwrap();
        assert_eq!(env.get("N"), Some(&"-1".to_string()));
    }

    #[test]
    fn validate_integer_accepts_unsigned_max() {
        let args = vec![arg("n", ArgType::Integer, true, "N")];
        let env = validate_call_input(r#"{"n": 18446744073709551615}"#, &args).unwrap();
        assert_eq!(env.get("N"), Some(&"18446744073709551615".to_string()));
    }

    #[test]
    fn validate_number_accepts_int_and_float() {
        let args = vec![arg("n", ArgType::Number, true, "N")];
        validate_call_input(r#"{"n": 42}"#, &args).unwrap();
        validate_call_input(r#"{"n": 3.14}"#, &args).unwrap();
    }

    #[test]
    fn validate_boolean_ok() {
        let args = vec![arg("b", ArgType::Boolean, true, "B")];
        let env = validate_call_input(r#"{"b": true}"#, &args).unwrap();
        assert_eq!(env.get("B"), Some(&"true".to_string()));
    }

    #[test]
    fn validate_malformed_json_errors() {
        let args = vec![arg("q", ArgType::String, true, "Q")];
        let err = validate_call_input("not json", &args).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"));
    }

    #[test]
    fn validate_non_object_errors() {
        let args = vec![];
        let err = validate_call_input(r#""string at top level""#, &args).unwrap_err();
        assert!(err.to_string().contains("must be a JSON object"));
    }

    #[test]
    fn validate_string_passes_special_chars_verbatim() {
        let args = vec![arg("q", ArgType::String, true, "Q")];
        let env = validate_call_input(r#"{"q": "foo\"; rm -rf /; #"}"#, &args).unwrap();
        assert_eq!(env.get("Q"), Some(&"foo\"; rm -rf /; #".to_string()));
    }

    #[test]
    fn validate_env_name_used_in_map_not_arg_name() {
        let args = vec![arg("queryString", ArgType::String, true, "QUERY")];
        let env = validate_call_input(r#"{"queryString": "x"}"#, &args).unwrap();
        assert!(env.contains_key("QUERY"));
        assert!(!env.contains_key("queryString"));
    }

    #[test]
    fn validate_empty_input_against_no_required() {
        let args = vec![arg("opt", ArgType::String, false, "OPT")];
        let env = validate_call_input(r#"{}"#, &args).unwrap();
        assert!(env.is_empty());
    }
}
