use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// --- AirlockChamber CRD ---

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "airlock.dev",
    version = "v1",
    kind = "AirlockChamber",
    namespaced,
    printcolumn = r#"{"name":"Image","type":"string","jsonPath":".spec.image"}"#,
    printcolumn = r#"{"name":"Keepalive","type":"boolean","jsonPath":".spec.keepalive"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    validation = "!has(self.spec.credentials) || self.spec.credentials.all(c, (has(c.env) && !has(c.file)) || (!has(c.env) && has(c.file)))"
)]
#[serde(rename_all = "camelCase")]
pub struct AirlockChamberSpec {
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
    pub host: String,
    pub port: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chamber_round_trip() {
        let json = serde_json::json!({
            "credentials": [{
                "secret": "git-ssh-key",
                "file": "/root/.ssh/id_ed25519"
            }],
            "egress": [{
                "host": "github.com",
                "port": 22
            }],
            "keepalive": false
        });

        let spec: AirlockChamberSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.credentials.len(), 1);
        assert_eq!(spec.credentials[0].secret, "git-ssh-key");
        assert_eq!(spec.egress.len(), 1);
        assert_eq!(spec.egress[0].host, "github.com");
        assert!(!spec.keepalive);

        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn chamber_defaults() {
        let json = serde_json::json!({});

        let spec: AirlockChamberSpec = serde_json::from_value(json).unwrap();
        assert!(spec.credentials.is_empty());
        assert!(spec.egress.is_empty());
        assert!(!spec.keepalive);
    }

    #[test]
    fn chamber_crd_generates() {
        use kube::CustomResourceExt;
        let crd = AirlockChamber::crd();
        assert_eq!(
            crd.metadata.name.as_deref(),
            Some("airlockchambers.airlock.dev")
        );
    }

    /// Credentialed chambers with keepalive=true are the load-bearing case for
    /// hot-path tools like git against private repos. Pod-lifecycle is not part
    /// of the chamber trust boundary; forbidding the combination would push
    /// users to abdicate airlock entirely. Lock the new behavior in.
    #[test]
    fn chamber_keepalive_with_credentials_is_allowed() {
        let json = serde_json::json!({
            "credentials": [{
                "secret": "git-ssh-key",
                "file": "/root/.ssh/id_ed25519"
            }],
            "keepalive": true
        });
        let spec: AirlockChamberSpec = serde_json::from_value(json).unwrap();
        assert!(spec.keepalive);
        assert_eq!(spec.credentials.len(), 1);
    }
}
