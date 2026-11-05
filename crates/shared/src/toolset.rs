//! Toolset config and tool-call validation. `CapabilityGrant` pairs one Secret
//! with one egress destination, `ToolsetConfig`/`ToolsetEntry` carry each
//! toolset's image, runtime shape and tools (merged live from the per-toolset
//! `ts-<name>` ConfigMaps the harness watches), and `validate_call_input`
//! checks an LLM's arguments against a tool's `ArgDecl`s.

use std::collections::{BTreeMap, HashMap, HashSet};

use k8s_openapi::api::core::v1::ConfigMap;
use serde::Deserialize;

/// Conventional mount path for the workspace PVC inside every tool Job.
/// Not configurable: tool images target `/workspace`.
pub const WORKSPACE_MOUNT_PATH: &str = "/workspace";

/// The projected ServiceAccount token mount every tool job depends on.
const SA_TOKEN_MOUNT_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// The image's dispatch entrypoint directory.
const DISPATCH_MOUNT_PATH: &str = "/etc/toolset";

/// One operator-approved credential, scoped to one (agent, toolset) pair.
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

/// The live toolset table, keyed by the `app.kubernetes.io/name` label of each
/// `ts-<name>` ConfigMap labelled `sycophant.md/type: toolset`.
#[derive(Clone, Default)]
pub struct ToolsetConfig {
    map: HashMap<String, ToolsetEntry>,
}

impl ToolsetConfig {
    /// Merge every toolset ConfigMap into one table. A ConfigMap that fails to
    /// parse is skipped and named in the returned errors.
    pub fn merge_configmaps<'a, I>(cms: I) -> (Self, Vec<String>)
    where
        I: IntoIterator<Item = &'a ConfigMap>,
    {
        let (map, errors) = crate::configmap_watcher::merge_labelled(cms, ToolsetEntry::from_data);
        (Self { map }, errors)
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
}

/// One toolset, parsed from its ConfigMap's flat data. It owns no credential
/// and no network hole.
///
/// `image` selects the tool job's pod; `keepalive` tells the reader when to reap
/// it. Neither is forwarded to the tool job. `env` forwards each key verbatim
/// into the tool job as an environment variable. `tools` are the tools the
/// image serves.
#[derive(Clone, Debug, Default)]
pub struct ToolsetEntry {
    pub image: Option<String>,
    pub keepalive: bool,
    pub deadline_seconds: Option<i64>,
    pub env: HashMap<String, String>,
    pub tools: Vec<ToolDecl>,
}

/// One tool a toolset serves: its LLM-facing name and description and its
/// declared args.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub args: Vec<ArgDecl>,
}

impl ToolDecl {
    /// The tool's JSON Schema `parameters` object, generated from its args.
    pub fn parameters_json(&self) -> String {
        let required: Vec<&str> = self
            .args
            .iter()
            .filter(|a| a.required)
            .map(|a| a.name.as_str())
            .collect();
        let properties: serde_json::Map<String, serde_json::Value> = self
            .args
            .iter()
            .map(|a| {
                let mut prop = serde_json::json!({"type": a.ty.as_schema_str()});
                if let Some(d) = &a.description {
                    prop["description"] = d.clone().into();
                }
                (a.name.clone(), prop)
            })
            .collect();
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": required,
            "properties": properties,
        })
        .to_string()
    }
}

/// One tool's args as flat fields, keyed by arg name then attribute.
type ArgFields<'a> = BTreeMap<&'a str, BTreeMap<&'a str, &'a str>>;

impl ToolsetEntry {
    /// Build one toolset from flat keys: `image`, `keepalive`,
    /// `deadlineSeconds`, `env.<NAME>`, `tool.<name>.description` and
    /// `tool.<name>.arg.<arg>.<type|required|env|description>`.
    fn from_data(data: &BTreeMap<String, String>) -> Result<Self, String> {
        let mut entry = ToolsetEntry::default();
        let mut tools: BTreeMap<&str, (String, ArgFields)> = BTreeMap::new();
        for (key, value) in data {
            match key.as_str() {
                "image" => entry.image = Some(value.clone()),
                "keepalive" => entry.keepalive = value == "true",
                "deadlineSeconds" => {
                    entry.deadline_seconds = Some(
                        value
                            .parse::<i64>()
                            .map_err(|_| format!("deadlineSeconds {value:?} is not a number"))?,
                    )
                }
                _ => {
                    if let Some(name) = key.strip_prefix("env.") {
                        entry.env.insert(name.to_string(), value.clone());
                        continue;
                    }
                    let unknown = || format!("unknown key {key}");
                    let rest = key.strip_prefix("tool.").ok_or_else(unknown)?;
                    let (tool, field) = rest.split_once('.').ok_or_else(unknown)?;
                    let slot = tools.entry(tool).or_default();
                    if field == "description" {
                        slot.0 = value.clone();
                    } else {
                        let (arg, attr) = field
                            .strip_prefix("arg.")
                            .and_then(|a| a.split_once('.'))
                            .ok_or_else(unknown)?;
                        slot.1.entry(arg).or_default().insert(attr, value);
                    }
                }
            }
        }
        for (name, (description, args)) in tools {
            let args = args
                .into_iter()
                .map(|(arg, attrs)| {
                    let field = |attr: &str| {
                        attrs
                            .get(attr)
                            .ok_or_else(|| format!("tool.{name}.arg.{arg}.{attr} is missing"))
                    };
                    let ty = field("type")?;
                    Ok(ArgDecl {
                        name: arg.to_string(),
                        ty: ArgType::from_schema_str(ty).ok_or_else(|| {
                            format!("tool.{name}.arg.{arg}.type {ty:?} is unknown")
                        })?,
                        required: attrs.get("required") == Some(&"true"),
                        env: field("env")?.to_string(),
                        description: attrs.get("description").map(|d| d.to_string()),
                    })
                })
                .collect::<Result<_, String>>()?;
            entry.tools.push(ToolDecl {
                name: name.to_string(),
                description,
                args,
            });
        }
        Ok(entry)
    }

    /// The `env` keys as environment pairs, in a stable order.
    pub fn forwarded_env(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
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
/// optional `description`. Feeds the per-agent harness dispatch producer.
#[derive(Debug, Clone, PartialEq)]
pub struct ArgDecl {
    pub name: String,
    pub ty: ArgType,
    pub required: bool,
    pub env: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    #[test]
    fn generated_parameters_match_stdlib_read_schema() {
        let cm = ConfigMap {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some("toolset-stdlib".into()),
                labels: Some(
                    [("app.kubernetes.io/name".to_string(), "stdlib".to_string())].into(),
                ),
                ..Default::default()
            },
            data: Some(
                [
                    ("image", "ghcr.io/sycophant-md/toolset@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
                    ("tool.Read.description", "Read a text file."),
                    ("tool.Read.arg.file_path.type", "string"),
                    ("tool.Read.arg.file_path.required", "true"),
                    ("tool.Read.arg.file_path.env", "path"),
                    ("tool.Read.arg.file_path.description", "File path."),
                    ("tool.Read.arg.offset.type", "integer"),
                    ("tool.Read.arg.offset.required", "false"),
                    ("tool.Read.arg.offset.env", "offset"),
                    ("tool.Read.arg.offset.description", "1-indexed starting line (default 1)."),
                    ("tool.Read.arg.limit.type", "integer"),
                    ("tool.Read.arg.limit.env", "limit"),
                    ("tool.Read.arg.limit.description", "Max lines to return (default unlimited)."),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ),
            ..Default::default()
        };
        let (config, errors) = ToolsetConfig::merge_configmaps([&cm]);
        assert!(errors.is_empty(), "{errors:?}");
        let read = config
            .get("stdlib")
            .unwrap()
            .tools
            .iter()
            .find(|t| t.name == "Read")
            .unwrap();

        let got: serde_json::Value = serde_json::from_str(&read.parameters_json()).unwrap();
        assert_eq!(
            got,
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["file_path"],
                "properties": {
                    "file_path": {"type": "string", "description": "File path."},
                    "offset": {"type": "integer", "description": "1-indexed starting line (default 1)."},
                    "limit": {"type": "integer", "description": "Max lines to return (default unlimited)."}
                }
            })
        );
    }

    #[test]
    fn flat_data_sets_runtime_fields_and_rejects_a_bad_deadline() {
        let mut data: BTreeMap<String, String> = [
            ("image", "ghcr.io/x/git@sha256:00"),
            ("keepalive", "true"),
            ("deadlineSeconds", "300"),
            ("env.GIT_TERMINAL_PROMPT", "0"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let entry = ToolsetEntry::from_data(&data).unwrap();
        assert_eq!(entry.image.as_deref(), Some("ghcr.io/x/git@sha256:00"));
        assert!(entry.keepalive);
        assert_eq!(entry.deadline_seconds, Some(300));
        assert_eq!(
            entry.forwarded_env(),
            vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())]
        );

        data.insert("deadlineSeconds".into(), "x".into());
        assert!(ToolsetEntry::from_data(&data).is_err());
    }

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

    /// (declared type, input value, expected env value or None for a
    /// wrong-type rejection).
    #[test]
    fn validate_passes_typed_values_to_env_and_rejects_wrong_types() {
        let cases = [
            (ArgType::Integer, r#""not a number""#, None),
            (ArgType::Integer, "1.5", None),
            (ArgType::Integer, "42", Some("42")),
            (ArgType::Integer, "-1", Some("-1")),
            (
                ArgType::Integer,
                "18446744073709551615",
                Some("18446744073709551615"),
            ),
            (ArgType::Number, "42", Some("42")),
            (ArgType::Number, "3.14", Some("3.14")),
            (ArgType::Boolean, "true", Some("true")),
        ];
        for (ty, value, want) in cases {
            let args = vec![arg("n", ty, true, "N")];
            let result = validate_call_input(&format!(r#"{{"n": {value}}}"#), &args);
            match want {
                Some(v) => assert_eq!(
                    result.unwrap().get("N").map(String::as_str),
                    Some(v),
                    "{value}"
                ),
                None => assert!(
                    result.unwrap_err().to_string().contains("wrong type"),
                    "{value}"
                ),
            }
        }
    }

    #[test]
    fn validate_malformed_json_errors() {
        let args = vec![arg("q", ArgType::String, true, "Q")];
        let err = validate_call_input("not json", &args).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"));
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
}
