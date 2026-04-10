use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "tightbeam.dev",
    version = "v1",
    kind = "TightbeamModel",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct TightbeamModelSpec {
    pub format: String,
    pub model: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SecretBinding {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
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
            format: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            thinking: Some("high".into()),
            secret: Some(SecretBinding {
                name: "anthropic-key".into(),
                env: Some("API_KEY".into()),
                file: None,
            }),
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"baseUrl\":\"https://api.anthropic.com/v1\""));
        assert!(json.contains("\"thinking\":\"high\""));
    }

    #[test]
    fn model_spec_deserializes_with_defaults() {
        let json = r#"{
            "format": "anthropic",
            "model": "claude-sonnet-4-20250514",
            "baseUrl": "https://api.anthropic.com/v1"
        }"#;
        let spec: TightbeamModelSpec = serde_json::from_str(json).unwrap();
        assert!(spec.thinking.is_none());
        assert!(spec.secret.is_none());
    }

    #[test]
    fn secret_binding_env_and_file() {
        let json = r#"{"name": "my-secret", "env": "API_KEY"}"#;
        let binding: SecretBinding = serde_json::from_str(json).unwrap();
        assert_eq!(binding.name, "my-secret");
        assert_eq!(binding.env.as_deref(), Some("API_KEY"));
        assert!(binding.file.is_none());

        let json = r#"{"name": "ssh-key", "file": "/root/.ssh/id_ed25519"}"#;
        let binding: SecretBinding = serde_json::from_str(json).unwrap();
        assert_eq!(binding.file.as_deref(), Some("/root/.ssh/id_ed25519"));
        assert!(binding.env.is_none());
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
}
