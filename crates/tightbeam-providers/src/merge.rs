use serde_json::{Map, Value};

/// Static reason text for a managed field clobbered by principal-supplied
/// params. Generic for fields not in the table; the controller-side audit
/// log keys on `field`, so the reason is principal-facing UX.
pub fn clobber_reason(field: &str) -> &'static str {
    match field {
        "model" => "operator binds the model identifier to the API key via TightbeamModel",
        "messages" | "contents" => "conversation history is owned by sycophant",
        "system" | "systemInstruction" => "system prompt is owned by sycophant",
        "tools" | "functionDeclarations" => "tool definitions are owned by sycophant",
        "stream" => "streaming is required for sycophant's audit log",
        _ => "operator-bound; principal override discarded",
    }
}

/// Apply an RFC 7396 JSON Merge Patch to `target` in place.
///
/// RFC 7396 §2: a `null` value in `patch` deletes the corresponding key in
/// `target`; nested objects merge recursively; arrays and scalars replace.
/// If `patch` is not a JSON object the RFC says "the result is the patch
/// itself" — under this signature (target is typed as `Map`) the closest
/// honest mapping is to clear the target.
/// Clone `params` (or an empty Map if None) into a body Map, then return it
/// alongside the result of `detect_clobbers` against the same managed list.
/// The caller writes managed values into the returned body last, after using
/// the returned `Vec<String>` to emit warnings.
pub fn build_managed_body(
    params: Option<&Map<String, Value>>,
    managed: &[&str],
) -> (Map<String, Value>, Vec<String>) {
    let body = params.cloned().unwrap_or_default();
    let clobbers = detect_clobbers(&body, managed);
    (body, clobbers)
}

/// Return the subset of `managed` keys that are present in `body`, in the
/// order they appear in `managed`, with no duplicates.
pub fn detect_clobbers(body: &Map<String, Value>, managed: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in managed {
        if body.contains_key(*key) && !out.iter().any(|existing| existing == *key) {
            out.push((*key).to_string());
        }
    }
    out
}

pub fn merge_rfc7396(target: &mut Map<String, Value>, patch: &Value) {
    let Value::Object(patch_obj) = patch else {
        target.clear();
        return;
    };

    for (key, value) in patch_obj {
        if value.is_null() {
            target.remove(key);
        } else if let Value::Object(_) = value {
            match target.get_mut(key) {
                Some(Value::Object(target_obj)) => merge_rfc7396(target_obj, value),
                _ => {
                    target.insert(key.clone(), value.clone());
                }
            }
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!("test fixture must be an object"),
        }
    }

    #[test]
    fn merge_adds_new_key() {
        let mut target = Map::new();
        merge_rfc7396(&mut target, &json!({"a": 1}));
        assert_eq!(target, obj(json!({"a": 1})));
    }

    #[test]
    fn merge_overwrites_existing_scalar() {
        let mut target = obj(json!({"a": 1}));
        merge_rfc7396(&mut target, &json!({"a": 2}));
        assert_eq!(target, obj(json!({"a": 2})));
    }

    #[test]
    fn merge_null_deletes_key() {
        let mut target = obj(json!({"a": 1, "b": 2}));
        merge_rfc7396(&mut target, &json!({"a": null}));
        assert!(!target.contains_key("a"), "key 'a' must be removed, not set to JSON null");
        assert_eq!(target, obj(json!({"b": 2})));
    }

    #[test]
    fn merge_null_on_missing_key_is_noop() {
        let mut target = Map::new();
        merge_rfc7396(&mut target, &json!({"a": null}));
        assert_eq!(target, Map::new());
    }

    #[test]
    fn merge_recurses_into_nested_object() {
        let mut target = obj(json!({"a": {"x": 1, "y": 2}}));
        merge_rfc7396(&mut target, &json!({"a": {"y": 99, "z": 3}}));
        assert_eq!(target, obj(json!({"a": {"x": 1, "y": 99, "z": 3}})));
    }

    #[test]
    fn merge_replaces_when_target_value_not_object() {
        let mut target = obj(json!({"a": 1}));
        merge_rfc7396(&mut target, &json!({"a": {"x": 2}}));
        assert_eq!(target, obj(json!({"a": {"x": 2}})));
    }

    #[test]
    fn merge_replaces_when_patch_value_not_object() {
        let mut target = obj(json!({"a": {"x": 1}}));
        merge_rfc7396(&mut target, &json!({"a": "scalar"}));
        assert_eq!(target, obj(json!({"a": "scalar"})));
    }

    #[test]
    fn merge_replaces_array_wholesale() {
        let mut target = obj(json!({"a": [1, 2, 3]}));
        merge_rfc7396(&mut target, &json!({"a": [9]}));
        assert_eq!(target, obj(json!({"a": [9]})));
    }

    #[test]
    fn merge_nested_null_deletes_inner_key() {
        let mut target = obj(json!({"a": {"x": 1, "y": 2}}));
        merge_rfc7396(&mut target, &json!({"a": {"x": null}}));
        assert_eq!(target, obj(json!({"a": {"y": 2}})));
    }

    #[test]
    fn merge_non_object_patch_clears_target() {
        let mut target = obj(json!({"a": 1}));
        merge_rfc7396(&mut target, &Value::String("scalar".into()));
        assert_eq!(target, Map::new());
    }

    #[test]
    fn merge_object_patch_with_no_keys_is_noop() {
        let mut target = obj(json!({"a": 1}));
        merge_rfc7396(&mut target, &json!({}));
        assert_eq!(target, obj(json!({"a": 1})));
    }

    #[test]
    fn detect_returns_empty_when_no_overlap() {
        let body = obj(json!({"foo": 1}));
        assert_eq!(detect_clobbers(&body, &["model", "messages"]), Vec::<String>::new());
    }

    #[test]
    fn detect_returns_only_present_keys() {
        let body = obj(json!({"model": "x", "foo": 1}));
        assert_eq!(detect_clobbers(&body, &["model", "messages"]), vec!["model"]);
    }

    #[test]
    fn detect_preserves_managed_input_order() {
        let body = obj(json!({"stream": true, "model": "x"}));
        assert_eq!(
            detect_clobbers(&body, &["model", "stream"]),
            vec!["model", "stream"]
        );
    }

    #[test]
    fn detect_no_duplicates_when_managed_repeats() {
        let body = obj(json!({"model": "x"}));
        assert_eq!(detect_clobbers(&body, &["model", "model"]), vec!["model"]);
    }

    #[test]
    fn detect_empty_managed_returns_empty() {
        let body = obj(json!({"model": "x"}));
        assert_eq!(detect_clobbers(&body, &[]), Vec::<String>::new());
    }

    #[test]
    fn detect_empty_body_returns_empty() {
        let body = Map::new();
        assert_eq!(detect_clobbers(&body, &["model"]), Vec::<String>::new());
    }

    #[test]
    fn build_with_none_params_returns_empty_body_and_no_clobbers() {
        let (body, clobbers) = build_managed_body(None, &["model"]);
        assert_eq!(body, Map::new());
        assert_eq!(clobbers, Vec::<String>::new());
    }

    #[test]
    fn build_clones_params_into_body() {
        let params = obj(json!({"temperature": 0.7}));
        let (body, clobbers) = build_managed_body(Some(&params), &["model"]);
        assert_eq!(body, obj(json!({"temperature": 0.7})));
        assert_eq!(clobbers, Vec::<String>::new());
    }

    #[test]
    fn build_detects_clobber_for_managed_key_in_params() {
        let params = obj(json!({"model": "x", "temperature": 0.7}));
        let (body, clobbers) = build_managed_body(Some(&params), &["model"]);
        assert_eq!(body, obj(json!({"model": "x", "temperature": 0.7})));
        assert_eq!(clobbers, vec!["model"]);
    }

    #[test]
    fn build_clobbers_in_managed_order() {
        let params = obj(json!({"stream": true, "model": "x"}));
        let (_body, clobbers) = build_managed_body(Some(&params), &["model", "stream"]);
        assert_eq!(clobbers, vec!["model", "stream"]);
    }

    #[test]
    fn clobber_reason_returns_distinct_text_per_managed_field() {
        let model = clobber_reason("model");
        let messages = clobber_reason("messages");
        let contents = clobber_reason("contents");
        let system = clobber_reason("system");
        let system_instr = clobber_reason("systemInstruction");
        let tools = clobber_reason("tools");
        let function_decls = clobber_reason("functionDeclarations");
        let stream = clobber_reason("stream");
        let unknown = clobber_reason("unknown");

        assert!(
            model.contains("operator binds the model identifier"),
            "model reason: {model}"
        );
        assert_eq!(messages, contents, "messages and contents share a reason");
        assert!(
            messages.contains("conversation history"),
            "messages reason: {messages}"
        );
        assert_eq!(system, system_instr);
        assert!(
            system.contains("system prompt"),
            "system reason: {system}"
        );
        assert_eq!(tools, function_decls);
        assert!(tools.contains("tool definitions"), "tools reason: {tools}");
        assert!(
            stream.contains("streaming"),
            "stream reason: {stream}"
        );
        assert!(
            unknown.contains("operator-bound"),
            "unknown reason: {unknown}"
        );

        // Each managed field has a distinct reason from the generic fallback.
        for r in [model, messages, system, tools, stream] {
            assert_ne!(r, unknown);
        }
    }

    #[test]
    fn build_does_not_mutate_caller_params() {
        let params = obj(json!({"a": 1}));
        let snapshot = params.clone();
        let _ = build_managed_body(Some(&params), &["model"]);
        assert_eq!(params, snapshot);
    }
}
