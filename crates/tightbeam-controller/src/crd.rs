use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// schemars/kube-derive default JsonSchema impl for serde_json::Value strips
// nested fields under k8s structural-schema rules. Override emits
// x-kubernetes-preserve-unknown-fields so the apiserver round-trips arbitrary
// nested params intact.
fn preserve_unknown_object(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    serde_json::from_value(serde_json::json!({
        "type": "object",
        "x-kubernetes-preserve-unknown-fields": true,
    }))
    .unwrap()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProviderRef {
    pub name: String,
}

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "tightbeam.dev",
    version = "v1",
    kind = "TightbeamModel",
    namespaced,
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".spec.providerRef.name"}"#,
    printcolumn = r#"{"name":"Model","type":"string","jsonPath":".spec.model"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct TightbeamModelSpec {
    pub provider_ref: ProviderRef,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "preserve_unknown_object")]
    pub params: Option<Map<String, Value>>,
}

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "tightbeam.dev",
    version = "v1",
    kind = "TightbeamProvider",
    namespaced,
    printcolumn = r#"{"name":"Format","type":"string","jsonPath":".spec.format"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct TightbeamProviderSpec {
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub secret: ProviderSecret,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProviderSecret {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "tightbeam.dev",
    version = "v1",
    kind = "TightbeamChannel",
    namespaced
)]
pub struct TightbeamChannelSpec {
    #[serde(rename = "type")]
    pub channel_type: String,
    #[serde(rename = "secretName")]
    pub secret_name: String,
    pub image: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::Resource;

    #[test]
    fn model_spec_serializes() {
        let spec = TightbeamModelSpec {
            provider_ref: ProviderRef {
                name: "anthropic".into(),
            },
            model: "claude-sonnet-4-20250514".into(),
            params: None,
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"providerRef\":{\"name\":\"anthropic\"}"));
        assert!(json.contains("\"model\":\"claude-sonnet-4-20250514\""));
    }

    #[test]
    fn model_spec_deserializes_minimal() {
        let json = r#"{
            "providerRef": { "name": "anthropic" },
            "model": "claude-sonnet-4-20250514"
        }"#;
        let spec: TightbeamModelSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.provider_ref.name, "anthropic");
        assert_eq!(spec.model, "claude-sonnet-4-20250514");
        assert!(spec.params.is_none());
    }

    #[test]
    fn model_spec_requires_provider_ref() {
        let json = r#"{ "model": "claude-sonnet-4-20250514" }"#;
        let result: Result<TightbeamModelSpec, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "TightbeamModelSpec must require providerRef"
        );
    }

    #[test]
    fn channel_spec_serializes() {
        let spec = TightbeamChannelSpec {
            channel_type: "discord".into(),
            secret_name: "discord-bot-token".into(),
            image: "ghcr.io/calebfaruki/tightbeam-channel-discord:latest".into(),
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"type\":\"discord\""));
        assert!(json.contains("\"secretName\":\"discord-bot-token\""));
    }

    #[test]
    fn channel_spec_deserializes_with_defaults() {
        let json = r#"{
            "type": "discord",
            "secretName": "token",
            "image": "ghcr.io/test:latest"
        }"#;
        let _spec: TightbeamChannelSpec = serde_json::from_str(json).unwrap();
    }

    #[test]
    fn model_crd_generates_correct_kind() {
        assert_eq!(TightbeamModel::kind(&()), "TightbeamModel");
        assert_eq!(TightbeamModel::group(&()), "tightbeam.dev");
        assert_eq!(TightbeamModel::version(&()), "v1");
    }

    #[test]
    fn channel_crd_generates_correct_kind() {
        assert_eq!(TightbeamChannel::kind(&()), "TightbeamChannel");
        assert_eq!(TightbeamChannel::group(&()), "tightbeam.dev");
        assert_eq!(TightbeamChannel::version(&()), "v1");
    }

    #[test]
    fn provider_spec_serializes_camel_case() {
        let spec = TightbeamProviderSpec {
            format: "anthropic".into(),
            base_url: Some("https://api.anthropic.com/v1".into()),
            secret: ProviderSecret {
                name: "anthropic-key".into(),
                key: Some("api-key".into()),
            },
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"baseUrl\":\"https://api.anthropic.com/v1\""));
        assert!(json.contains("\"format\":\"anthropic\""));
    }

    #[test]
    fn provider_spec_deserializes_with_optional_base_url_omitted() {
        let json = r#"{
            "format": "anthropic",
            "secret": { "name": "anthropic-key" }
        }"#;
        let spec: TightbeamProviderSpec = serde_json::from_str(json).unwrap();
        assert!(spec.base_url.is_none());
    }

    #[test]
    fn provider_spec_deserializes_with_optional_secret_key_omitted() {
        let json = r#"{
            "format": "anthropic",
            "secret": { "name": "anthropic-key" }
        }"#;
        let spec: TightbeamProviderSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.secret.name, "anthropic-key");
        assert!(spec.secret.key.is_none());
    }

    #[test]
    fn provider_spec_requires_secret() {
        let json = r#"{ "format": "anthropic" }"#;
        let result: Result<TightbeamProviderSpec, _> = serde_json::from_str(json);
        assert!(result.is_err(), "TightbeamProviderSpec must require a secret");
    }

    #[test]
    fn provider_crd_generates_correct_kind() {
        assert_eq!(TightbeamProvider::kind(&()), "TightbeamProvider");
        assert_eq!(TightbeamProvider::group(&()), "tightbeam.dev");
        assert_eq!(TightbeamProvider::version(&()), "v1");
    }

    #[test]
    fn model_spec_deserializes_with_params() {
        let json = r#"{
            "providerRef": { "name": "anthropic" },
            "model": "claude-sonnet-4-20250514",
            "params": {
                "output_config": { "effort": "high" },
                "max_tokens": 16000
            }
        }"#;
        let spec: TightbeamModelSpec = serde_json::from_str(json).unwrap();
        let params = spec.params.expect("params must deserialize");
        assert_eq!(
            params.get("output_config").and_then(|v| v.get("effort")),
            Some(&Value::String("high".into()))
        );
        assert_eq!(
            params.get("max_tokens"),
            Some(&Value::Number(16000.into()))
        );
    }

}
