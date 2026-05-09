use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "sycophant.md",
    version = "v1",
    kind = "Mainframe",
    namespaced,
    status = "MainframeStatus",
    printcolumn = r#"{"name":"Kind","type":"string","jsonPath":".spec.source.kind"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MainframeSpec {
    pub source: MainframeSource,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MainframeSource {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_path: Option<HostPathSource>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostPathSource {
    pub path: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MainframeStatus {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<MainframeCondition>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MainframeCondition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    pub reason: String,
    pub message: String,
    pub last_transition_time: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainframe_hostpath_round_trip() {
        let json = serde_json::json!({
            "source": {
                "kind": "HostPath",
                "hostPath": {
                    "path": "/home/operator/sycophant/workspaces/foo"
                }
            }
        });

        let spec: MainframeSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.source.kind, "HostPath");
        let hp = spec
            .source
            .host_path
            .as_ref()
            .expect("hostPath block required for kind HostPath");
        assert_eq!(hp.path, "/home/operator/sycophant/workspaces/foo");

        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn mainframe_source_omits_hostpath_when_absent() {
        let json = serde_json::json!({ "source": { "kind": "Unknown" } });
        let spec: MainframeSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.source.kind, "Unknown");
        assert!(spec.source.host_path.is_none());
        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn mainframe_crd_generates() {
        use kube::CustomResourceExt;
        let crd = Mainframe::crd();
        assert_eq!(
            crd.metadata.name.as_deref(),
            Some("mainframes.sycophant.md")
        );
    }

    /// Future kinds (S3, OCI, git, lakeFS, ...) extend `MainframeSource` by
    /// adding optional sibling blocks under `source`. The schema must keep
    /// `kind` and `hostPath` exposed so the discriminator stays observable.
    #[test]
    fn mainframe_crd_schema_exposes_kind_and_hostpath() {
        use kube::CustomResourceExt;
        let crd = Mainframe::crd();
        let version = crd.spec.versions.first().expect("CRD must have versions");
        let validation = version.schema.as_ref().expect("schema must be present");
        let openapi = validation
            .open_api_v3_schema
            .as_ref()
            .expect("openAPIV3Schema must be present");
        let spec_props = openapi
            .properties
            .as_ref()
            .expect("schema must have top-level properties")
            .get("spec")
            .expect("schema must have a spec property");
        let source = spec_props
            .properties
            .as_ref()
            .expect("spec must have properties")
            .get("source")
            .expect("spec must have a source property");
        let source_props = source
            .properties
            .as_ref()
            .expect("source must have properties");
        assert!(
            source_props.contains_key("kind"),
            "source must have a kind discriminator"
        );
        let host_path = source_props
            .get("hostPath")
            .expect("source must have a hostPath block");
        let host_path_props = host_path
            .properties
            .as_ref()
            .expect("hostPath must have properties");
        assert!(
            host_path_props.contains_key("path"),
            "hostPath must have a path field"
        );
    }
}
