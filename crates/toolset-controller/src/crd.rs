use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// --- Toolset CRD (tool dispatch) ---

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "sycophant.md",
    version = "v1",
    kind = "Toolset",
    namespaced,
    printcolumn = r#"{"name":"Image","type":"string","jsonPath":".spec.image"}"#,
    printcolumn = r#"{"name":"Keepalive","type":"boolean","jsonPath":".spec.keepalive"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    validation = "!has(self.spec.credentials) || self.spec.credentials.all(c, (has(c.env) && !has(c.file)) || (!has(c.env) && has(c.file)))"
)]
#[serde(rename_all = "camelCase")]
pub struct ToolsetSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<CredentialMapping>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress: Vec<EgressRule>,

    #[serde(default)]
    pub keepalive: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct CredentialMapping {
    pub secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct EgressRule {
    /// Trust principal for the egress: the registrable domain (e.g.
    /// `notion.com`, `github.com`) whose zone the toolset is allowed
    /// to reach. The chart trusts the apex AND its single-label
    /// subdomain space (`*.notion.com`), so a `domain: notion.com`
    /// declaration covers `api.notion.com`, `auth.notion.com`, etc.
    /// The same principal controls the whole subtree; enumerating
    /// individual subdomains adds friction without security gain.
    pub domain: String,
    pub port: u16,
}

// --- Model / Provider CRDs (turn dispatch) ---

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
    group = "sycophant.md",
    version = "v1",
    kind = "Model",
    namespaced,
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".spec.providerRef.name"}"#,
    printcolumn = r#"{"name":"Model","type":"string","jsonPath":".spec.model"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub provider_ref: ProviderRef,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "preserve_unknown_object")]
    pub params: Option<Map<String, Value>>,
}

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "sycophant.md",
    version = "v1",
    kind = "Provider",
    namespaced,
    printcolumn = r#"{"name":"Format","type":"string","jsonPath":".spec.format"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSpec {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolset_round_trip() {
        let json = serde_json::json!({
            "credentials": [{
                "secret": "git-ssh-key",
                "file": "/root/.ssh/id_ed25519"
            }],
            "egress": [{
                "domain": "github.com",
                "port": 22
            }],
            "keepalive": false
        });

        let spec: ToolsetSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.credentials.len(), 1);
        assert_eq!(spec.credentials[0].secret, "git-ssh-key");
        assert_eq!(spec.egress.len(), 1);
        assert_eq!(spec.egress[0].domain, "github.com");
        assert!(!spec.keepalive);

        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn toolset_defaults() {
        let json = serde_json::json!({});

        let spec: ToolsetSpec = serde_json::from_value(json).unwrap();
        assert!(spec.credentials.is_empty());
        assert!(spec.egress.is_empty());
        assert!(!spec.keepalive);
    }

    #[test]
    fn toolset_crd_generates() {
        use kube::CustomResourceExt;
        let crd = Toolset::crd();
        assert_eq!(crd.metadata.name.as_deref(), Some("toolsets.sycophant.md"));
    }

    /// Credentialed toolsets with keepalive=true are the load-bearing case for
    /// hot-path tools like git against private repos. Pod-lifecycle is not part
    /// of the toolset trust boundary; forbidding the combination would push
    /// users to abdicate the toolset entirely. Lock the behavior in.
    #[test]
    fn toolset_keepalive_with_credentials_is_allowed() {
        let json = serde_json::json!({
            "credentials": [{
                "secret": "git-ssh-key",
                "file": "/root/.ssh/id_ed25519"
            }],
            "keepalive": true
        });
        let spec: ToolsetSpec = serde_json::from_value(json).unwrap();
        assert!(spec.keepalive);
        assert_eq!(spec.credentials.len(), 1);
    }

    #[test]
    fn model_spec_serializes() {
        let spec = ModelSpec {
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
        let spec: ModelSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.provider_ref.name, "anthropic");
        assert_eq!(spec.model, "claude-sonnet-4-20250514");
        assert!(spec.params.is_none());
    }

    #[test]
    fn model_spec_requires_provider_ref() {
        let json = r#"{ "model": "claude-sonnet-4-20250514" }"#;
        let result: Result<ModelSpec, _> = serde_json::from_str(json);
        assert!(result.is_err(), "ModelSpec must require providerRef");
    }

    #[test]
    fn model_crd_generates_correct_kind() {
        use kube::Resource;
        assert_eq!(Model::kind(&()), "Model");
        assert_eq!(Model::group(&()), "sycophant.md");
        assert_eq!(Model::version(&()), "v1");
    }

    #[test]
    fn provider_spec_serializes_camel_case() {
        let spec = ProviderSpec {
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
        let spec: ProviderSpec = serde_json::from_str(json).unwrap();
        assert!(spec.base_url.is_none());
    }

    #[test]
    fn provider_spec_deserializes_with_optional_secret_key_omitted() {
        let json = r#"{
            "format": "anthropic",
            "secret": { "name": "anthropic-key" }
        }"#;
        let spec: ProviderSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.secret.name, "anthropic-key");
        assert!(spec.secret.key.is_none());
    }

    #[test]
    fn provider_spec_requires_secret() {
        let json = r#"{ "format": "anthropic" }"#;
        let result: Result<ProviderSpec, _> = serde_json::from_str(json);
        assert!(result.is_err(), "ProviderSpec must require a secret");
    }

    #[test]
    fn provider_crd_generates_correct_kind() {
        use kube::Resource;
        assert_eq!(Provider::kind(&()), "Provider");
        assert_eq!(Provider::group(&()), "sycophant.md");
        assert_eq!(Provider::version(&()), "v1");
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
        let spec: ModelSpec = serde_json::from_str(json).unwrap();
        let params = spec.params.expect("params must deserialize");
        assert_eq!(
            params.get("output_config").and_then(|v| v.get("effort")),
            Some(&Value::String("high".into()))
        );
        assert_eq!(params.get("max_tokens"), Some(&Value::Number(16000.into())));
    }

    /// `preserve_unknown_object` emits `x-kubernetes-preserve-unknown-fields: true`
    /// for the `params` field. Without it, kube-apiserver strips nested keys from
    /// `params` on PUT, silently corrupting operator pass-through.
    #[test]
    fn model_spec_params_schema_preserves_unknown_fields() {
        use kube::CustomResourceExt;
        let crd = Model::crd();
        let version = crd
            .spec
            .versions
            .first()
            .expect("CRD must declare at least one version");
        let validation = version.schema.as_ref().expect("version must have a schema");
        let openapi = validation
            .open_api_v3_schema
            .as_ref()
            .expect("schema must have openAPIV3Schema");
        let spec_schema = openapi
            .properties
            .as_ref()
            .expect("schema must have top-level properties")
            .get("spec")
            .expect("schema must have a spec property");
        let params_schema = spec_schema
            .properties
            .as_ref()
            .expect("spec must have properties")
            .get("params")
            .expect("spec must have a params property");
        assert_eq!(
            params_schema.x_kubernetes_preserve_unknown_fields,
            Some(true),
            "params must preserve unknown fields so operator-supplied nested params survive PUT"
        );
    }
}
