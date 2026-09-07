use crate::registry::ArgDecl;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ArgType;

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
}
