//! Per-turn params overlay: an RFC 7396 (JSON Merge Patch) recursive merge of
//! frontmatter over the Model's static params.

use crate::state::ControllerState;
use serde_json::{Map, Value};

/// Recursively apply `patch` to `target` per RFC 7396 (JSON Merge Patch): an
/// object key set to null removes it; a nested object recurses; any other
/// value replaces. A non-object patch clears the target.
pub fn merge_rfc7396(target: &mut Map<String, Value>, patch: &Value) {
    let Value::Object(patch_obj) = patch else {
        target.clear();
        return;
    };
    for (key, value) in patch_obj {
        if value.is_null() {
            target.remove(key);
        } else if value.is_object() {
            match target.get_mut(key) {
                Some(Value::Object(t)) => merge_rfc7396(t, value),
                _ => {
                    target.insert(key.clone(), value.clone());
                }
            }
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// Build the RFC 7396-merged `params_json` blob for a turn: the Model's static
/// params with any per-turn `frontmatter_params` merged over them. Returns
/// `None` when the merged map is empty. The caller passes `None` frontmatter
/// today (the harness strips and resolves frontmatter before dispatch).
pub async fn build_params_json(
    state: &ControllerState,
    model: &str,
    frontmatter_params: Option<&Map<String, Value>>,
) -> Option<String> {
    let model_spec = state.get_model_spec(model).await;
    let mut merged = model_spec.and_then(|s| s.params).unwrap_or_default();
    if let Some(fm_params) = frontmatter_params {
        merge_rfc7396(&mut merged, &Value::Object(fm_params.clone()));
    }
    if merged.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&merged)
                .expect("Map<String, Value> serializes deterministically"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{ModelSpec, ProviderRef};
    use std::sync::Arc;

    fn make_state() -> Arc<ControllerState> {
        ControllerState::new(
            None,
            "default".into(),
            "http://localhost:9090".into(),
            "ghcr.io/test/prompt-job:latest".into(),
            shared::scheduling::SchedulingConfig::default(),
        )
    }

    fn model_spec(params: Option<Map<String, Value>>) -> ModelSpec {
        ModelSpec {
            provider_ref: ProviderRef {
                name: "anthropic".into(),
            },
            model: "claude".into(),
            params,
        }
    }

    #[tokio::test]
    async fn params_json_none_when_neither_set() {
        let state = make_state();
        state.set_model_spec("m".into(), model_spec(None)).await;
        assert!(build_params_json(&state, "m", None).await.is_none());
    }

    #[tokio::test]
    async fn params_json_carries_model_params_when_only_model_set() {
        let state = make_state();
        let mut params = Map::new();
        params.insert("temperature".into(), serde_json::json!(0.7));
        state
            .set_model_spec("m".into(), model_spec(Some(params)))
            .await;
        let result = build_params_json(&state, "m", None).await.expect("Some");
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed.get("temperature"), Some(&serde_json::json!(0.7)));
    }

    #[tokio::test]
    async fn params_json_merges_frontmatter_over_model_via_rfc7396() {
        let state = make_state();
        let mut model_params = Map::new();
        model_params.insert("output_config".into(), serde_json::json!({"effort": "low"}));
        state
            .set_model_spec("m".into(), model_spec(Some(model_params)))
            .await;

        let mut fm_params = Map::new();
        fm_params.insert("output_config".into(), serde_json::json!({"effort": "max"}));

        let result = build_params_json(&state, "m", Some(&fm_params))
            .await
            .expect("Some");
        let parsed: Value = serde_json::from_str(&result).unwrap();
        // RFC 7396 recursive merge: frontmatter wins for `effort`.
        assert_eq!(
            parsed.get("output_config").and_then(|v| v.get("effort")),
            Some(&serde_json::json!("max"))
        );
    }
}
