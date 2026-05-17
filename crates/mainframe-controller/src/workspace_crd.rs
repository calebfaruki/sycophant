use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use shared::storage::{HostPathSpec, S3Spec};

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "sycophant.md",
    version = "v1",
    kind = "Workspace",
    shortname = "mfw",
    namespaced,
    status = "WorkspaceStatus",
    printcolumn = r#"{"name":"Image","type":"string","jsonPath":".spec.image"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSpec {
    pub image: String,
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    /// Inline mainframe kernel content. Mirrors the existing
    /// `.Values.workspaces[*].mainframe` shape so the chart can render
    /// Workspace CRs from current values without reshaping operator input.
    /// In Stage 3, mainframe-controller materializes the Kernel CR from
    /// this block when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mainframe: Option<WorkspaceMainframe>,
    /// Bare-string references to existing Kernel CRs in the same
    /// namespace. The workspace mounts each referenced kernel at the
    /// expected mainframe path. Empty by default; populated when the
    /// chart wires `mainframe` inline (Stage 3 will add the workspace
    /// name automatically) or when the operator wants to share a kernel
    /// across workspaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kernels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chambers: Vec<String>,
}

/// Inline kernel content carried on a Workspace.spec. Structurally
/// identical to KernelSpec; kept as its own type so changes to one do
/// not implicitly bleed into the other.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMainframe {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_path: Option<HostPathSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3: Option<S3Spec>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<WorkspaceCondition>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCondition {
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
    use shared::storage::SecretRef;

    #[test]
    fn workspace_crd_generates() {
        use kube::CustomResourceExt;
        let crd = Workspace::crd();
        assert_eq!(
            crd.metadata.name.as_deref(),
            Some("workspaces.sycophant.md")
        );
    }

    #[test]
    fn workspace_crd_declares_mfw_shortname() {
        use kube::CustomResourceExt;
        let crd = Workspace::crd();
        let short_names = crd
            .spec
            .names
            .short_names
            .as_ref()
            .expect("Workspace CRD must declare shortNames");
        assert!(
            short_names.iter().any(|s| s == "mfw"),
            "Workspace CRD must declare the `mfw` shortname (got {short_names:?})"
        );
    }

    #[test]
    fn workspace_minimal_spec_round_trip() {
        let json = serde_json::json!({
            "image": "ghcr.io/calebfaruki/transponder",
            "tag": "v0.1"
        });
        let spec: WorkspaceSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.image, "ghcr.io/calebfaruki/transponder");
        assert_eq!(spec.tag, "v0.1");
        assert!(spec.cpu.is_none());
        assert!(spec.memory.is_none());
        assert!(spec.storage.is_none());
        assert!(spec.mainframe.is_none());
        assert!(spec.kernels.is_empty());
        assert!(spec.chambers.is_empty());
        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn workspace_spec_capabilities_round_trip() {
        let json = serde_json::json!({
            "image": "ghcr.io/calebfaruki/transponder",
            "tag": "v0.1",
            "kernels": ["agents-md"],
            "chambers": ["git-ops", "notion-cli-ro"]
        });
        let spec: WorkspaceSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.kernels, vec!["agents-md".to_string()]);
        assert_eq!(
            spec.chambers,
            vec!["git-ops".to_string(), "notion-cli-ro".to_string()]
        );
        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn workspace_resources_optional_round_trip() {
        let json = serde_json::json!({
            "image": "ghcr.io/calebfaruki/transponder",
            "tag": "v0.1",
            "cpu": "0.5",
            "memory": "1Gi",
            "storage": "5Gi"
        });
        let spec: WorkspaceSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.cpu.as_deref(), Some("0.5"));
        assert_eq!(spec.memory.as_deref(), Some("1Gi"));
        assert_eq!(spec.storage.as_deref(), Some("5Gi"));
        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn workspace_resources_skip_serializing_when_absent() {
        let spec = WorkspaceSpec {
            image: "img".into(),
            tag: "t".into(),
            pull_policy: None,
            cpu: None,
            memory: None,
            storage: None,
            mainframe: None,
            kernels: vec![],
            chambers: vec![],
        };
        let json = serde_json::to_value(&spec).unwrap();
        let obj = json.as_object().expect("spec must serialize as object");
        assert!(!obj.contains_key("cpu"));
        assert!(!obj.contains_key("memory"));
        assert!(!obj.contains_key("storage"));
        assert!(!obj.contains_key("pullPolicy"));
        assert!(!obj.contains_key("mainframe"));
        assert!(!obj.contains_key("kernels"));
        assert!(!obj.contains_key("chambers"));
    }

    #[test]
    fn workspace_mainframe_hostpath_round_trip() {
        let json = serde_json::json!({
            "image": "img",
            "tag": "t",
            "mainframe": {
                "kind": "HostPath",
                "hostPath": { "path": "/home/me/sycophant/foo" }
            }
        });
        let spec: WorkspaceSpec = serde_json::from_value(json.clone()).unwrap();
        let mf = spec.mainframe.as_ref().expect("mainframe block present");
        assert_eq!(mf.kind, "HostPath");
        let hp = mf.host_path.as_ref().expect("hostPath block present");
        assert_eq!(hp.path, "/home/me/sycophant/foo");
        assert!(mf.s3.is_none());
        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn workspace_mainframe_s3_round_trip() {
        let json = serde_json::json!({
            "image": "img",
            "tag": "t",
            "mainframe": {
                "kind": "S3",
                "s3": {
                    "endpoint": "http://versitygw:7070",
                    "bucket": "sycophant-tenants",
                    "prefix": "tenant-abc/mainframe/",
                    "region": "us-east-1",
                    "forcePathStyle": true,
                    "credentials": { "name": "tenant-s3-credentials" }
                }
            }
        });
        let spec: WorkspaceSpec = serde_json::from_value(json.clone()).unwrap();
        let mf = spec.mainframe.as_ref().expect("mainframe block present");
        assert_eq!(mf.kind, "S3");
        let s3 = mf.s3.as_ref().expect("s3 block present");
        assert_eq!(s3.bucket, "sycophant-tenants");
        let creds = s3.credentials.as_ref().expect("credentials present");
        assert_eq!(creds.name, "tenant-s3-credentials");
        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn workspace_spec_constructable_directly() {
        // Compile-time check that callers (e.g., the materialization
        // logic shipping in Stage 3) can construct a WorkspaceSpec
        // from in-process values without going through serde.
        let _ = WorkspaceSpec {
            image: "img".into(),
            tag: "t".into(),
            pull_policy: Some("IfNotPresent".into()),
            cpu: Some("0.5".into()),
            memory: Some("1Gi".into()),
            storage: None,
            mainframe: Some(WorkspaceMainframe {
                kind: "S3".into(),
                host_path: None,
                s3: Some(S3Spec {
                    endpoint: "http://x".into(),
                    bucket: "b".into(),
                    prefix: "p/".into(),
                    region: "us-east-1".into(),
                    force_path_style: false,
                    credentials: Some(SecretRef {
                        name: "creds".into(),
                        access_key_id_key: None,
                        secret_access_key_key: None,
                    }),
                }),
            }),
            kernels: vec!["agents-md".into()],
            chambers: vec![],
        };
    }
}
