use std::collections::{HashMap, HashSet};

use tonic::Status;

use crate::registry::{ArgDecl, ArgType};

/// Synthesize a JSON Schema string for the LLM tool-definition surface. The
/// schema describes the tool's input shape: an object with one property per
/// declared arg, marked required when `arg.required`.
pub fn synthesize_schema(args: &[ArgDecl]) -> String {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for arg in args {
        let mut prop = serde_json::Map::new();
        prop.insert(
            "type".to_string(),
            serde_json::Value::String(arg.ty.as_schema_str().to_string()),
        );
        if let Some(desc) = &arg.description {
            prop.insert(
                "description".to_string(),
                serde_json::Value::String(desc.clone()),
            );
        }
        properties.insert(arg.name.clone(), serde_json::Value::Object(prop));
        if arg.required {
            required.push(serde_json::Value::String(arg.name.clone()));
        }
    }
    let schema = serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    });
    schema.to_string()
}

/// Validate an LLM-provided `input_json` payload against a tool's declared
/// `args`. On success, returns a map of `env_var_name -> value_as_string`
/// — the env vars the runtime will set on the `make` invocation. The map
/// only contains entries the LLM provided (optional+missing args are absent
/// from the result; they do not appear as empty env vars).
pub fn validate_call_input(
    input_json: &str,
    args: &[ArgDecl],
) -> Result<HashMap<String, String>, Status> {
    let parsed: serde_json::Value = serde_json::from_str(input_json)
        .map_err(|e| Status::invalid_argument(format!("input is not valid JSON: {e}")))?;

    let obj = parsed
        .as_object()
        .ok_or_else(|| Status::invalid_argument("input must be a JSON object"))?;

    let declared: HashSet<&str> = args.iter().map(|a| a.name.as_str()).collect();
    for key in obj.keys() {
        if !declared.contains(key.as_str()) {
            return Err(Status::invalid_argument(format!(
                "unknown input field '{key}' (declared: {declared:?})"
            )));
        }
    }

    let mut env_map = HashMap::new();
    for arg in args {
        let Some(value) = obj.get(&arg.name) else {
            if arg.required {
                return Err(Status::invalid_argument(format!(
                    "missing required input field '{}'",
                    arg.name
                )));
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
                return Err(Status::invalid_argument(format!(
                    "input field '{}' has wrong type (expected {})",
                    arg.name,
                    arg.ty.as_schema_str()
                )));
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

    fn arg_with_desc(name: &str, ty: ArgType, required: bool, env: &str, desc: &str) -> ArgDecl {
        ArgDecl {
            name: name.to_string(),
            ty,
            required,
            env: env.to_string(),
            description: Some(desc.to_string()),
        }
    }

    // --- synthesize_schema ---

    #[test]
    fn schema_empty_args() {
        let schema: serde_json::Value = serde_json::from_str(&synthesize_schema(&[])).unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].as_object().unwrap().is_empty());
        assert!(schema["required"].as_array().unwrap().is_empty());
    }

    #[test]
    fn schema_required_string() {
        let s = synthesize_schema(&[arg("query", ArgType::String, true, "QUERY")]);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["properties"]["query"]["type"], "string");
        assert_eq!(v["required"][0], "query");
    }

    #[test]
    fn schema_optional_not_in_required() {
        let s = synthesize_schema(&[arg("filter", ArgType::String, false, "FILTER")]);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["properties"]["filter"]["type"], "string");
        assert!(v["required"].as_array().unwrap().is_empty());
    }

    #[test]
    fn schema_description_roundtrip() {
        let s = synthesize_schema(&[arg_with_desc(
            "page_id",
            ArgType::String,
            true,
            "PAGE_ID",
            "The Notion page ID",
        )]);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(
            v["properties"]["page_id"]["description"],
            "The Notion page ID"
        );
    }

    #[test]
    fn schema_all_types() {
        let s = synthesize_schema(&[
            arg("s", ArgType::String, true, "S"),
            arg("i", ArgType::Integer, true, "I"),
            arg("n", ArgType::Number, true, "N"),
            arg("b", ArgType::Boolean, true, "B"),
        ]);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["properties"]["s"]["type"], "string");
        assert_eq!(v["properties"]["i"]["type"], "integer");
        assert_eq!(v["properties"]["n"]["type"], "number");
        assert_eq!(v["properties"]["b"]["type"], "boolean");
    }

    // --- validate_call_input ---

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
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err
            .message()
            .contains("missing required input field 'query'"));
    }

    #[test]
    fn validate_unknown_field_errors() {
        let args = vec![arg("query", ArgType::String, true, "QUERY")];
        let err = validate_call_input(r#"{"query": "x", "bogus": "y"}"#, &args).unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("unknown input field 'bogus'"));
    }

    #[test]
    fn validate_wrong_type_string_for_integer_errors() {
        let args = vec![arg("n", ArgType::Integer, true, "N")];
        let err = validate_call_input(r#"{"n": "not a number"}"#, &args).unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("wrong type"));
    }

    #[test]
    fn validate_integer_rejects_float() {
        let args = vec![arg("n", ArgType::Integer, true, "N")];
        let err = validate_call_input(r#"{"n": 1.5}"#, &args).unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn validate_integer_accepts_positive() {
        let args = vec![arg("n", ArgType::Integer, true, "N")];
        let env = validate_call_input(r#"{"n": 42}"#, &args).unwrap();
        assert_eq!(env.get("N"), Some(&"42".to_string()));
    }

    #[test]
    fn validate_integer_accepts_negative() {
        // Negative i64 is not u64; catches `||` → `&&` mutation in the
        // `is_i64() || is_u64()` guard.
        let args = vec![arg("n", ArgType::Integer, true, "N")];
        let env = validate_call_input(r#"{"n": -1}"#, &args).unwrap();
        assert_eq!(env.get("N"), Some(&"-1".to_string()));
    }

    #[test]
    fn validate_integer_accepts_unsigned_max() {
        // u64::MAX exceeds i64::MAX; catches `||` → `&&` from the other side.
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
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("not valid JSON"));
    }

    #[test]
    fn validate_non_object_errors() {
        let args = vec![];
        let err = validate_call_input(r#""string at top level""#, &args).unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("must be a JSON object"));
    }

    #[test]
    fn validate_string_passes_special_chars_verbatim() {
        // Critical for security: the value is passed unchanged. The runtime
        // sets this as an env var; the Makefile recipe references it via
        // `"$$VAR"` which is single-token shell-quoted. Special characters
        // never re-parse.
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
