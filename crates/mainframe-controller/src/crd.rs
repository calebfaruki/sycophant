use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use shared::storage::{HostPathSpec, S3Spec};

/// `kind` discriminator allowlist. Apply-time rejection of unknown
/// strings keeps a typo (`hostpath` vs `HostPath`) from quietly landing
/// a no-op Kernel.
fn kernel_kind_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    serde_json::from_value(serde_json::json!({
        "type": "string",
        "enum": ["HostPath", "S3"],
    }))
    .unwrap()
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "sycophant.md",
    version = "v1",
    kind = "Kernel",
    shortname = "mfk",
    namespaced,
    status = "KernelStatus",
    printcolumn = r#"{"name":"Kind","type":"string","jsonPath":".spec.kind"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KernelSpec {
    #[schemars(schema_with = "kernel_kind_schema")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_path: Option<HostPathSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3: Option<S3Spec>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KernelStatus {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<KernelCondition>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KernelCondition {
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
    fn kernel_hostpath_round_trip() {
        let json = serde_json::json!({
            "kind": "HostPath",
            "hostPath": {
                "path": "/home/operator/sycophant/workspaces/foo"
            }
        });

        let spec: KernelSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.kind, "HostPath");
        let hp = spec
            .host_path
            .as_ref()
            .expect("hostPath block required for kind HostPath");
        assert_eq!(hp.path, "/home/operator/sycophant/workspaces/foo");
        assert!(spec.s3.is_none());

        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn kernel_s3_round_trip() {
        let json = serde_json::json!({
            "kind": "S3",
            "s3": {
                "endpoint": "http://versitygw:7070",
                "bucket": "sycophant-tenants",
                "prefix": "tenant-abc/mainframe/",
                "region": "us-east-1",
                "forcePathStyle": true,
                "credentials": { "name": "tenant-s3-credentials" }
            }
        });

        let spec: KernelSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.kind, "S3");
        let s3 = spec.s3.as_ref().expect("s3 block required for kind S3");
        assert_eq!(s3.endpoint, "http://versitygw:7070");
        assert_eq!(s3.bucket, "sycophant-tenants");
        assert_eq!(s3.prefix, "tenant-abc/mainframe/");
        assert!(s3.force_path_style);
        assert_eq!(
            s3.credentials
                .as_ref()
                .expect("Mainframe Kernel S3 source must carry credentials")
                .name,
            "tenant-s3-credentials"
        );
        assert!(spec.host_path.is_none());

        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn kernel_omits_blocks_when_absent() {
        let json = serde_json::json!({ "kind": "Unknown" });
        let spec: KernelSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.kind, "Unknown");
        assert!(spec.host_path.is_none());
        assert!(spec.s3.is_none());
        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn kernel_crd_generates() {
        use kube::CustomResourceExt;
        let crd = Kernel::crd();
        assert_eq!(crd.metadata.name.as_deref(), Some("kernels.sycophant.md"));
    }

    #[test]
    fn kernel_crd_declares_mfk_shortname() {
        use kube::CustomResourceExt;
        let crd = Kernel::crd();
        let short_names = crd
            .spec
            .names
            .short_names
            .as_ref()
            .expect("Kernel CRD must declare shortNames");
        assert!(
            short_names.iter().any(|s| s == "mfk"),
            "Kernel CRD must declare the `mfk` shortname (got {short_names:?})"
        );
    }

    /// Future kinds extend `KernelSpec` by adding optional sibling blocks
    /// alongside `kind`, `hostPath`, and `s3`. The schema must keep all
    /// known sibling blocks exposed so the discriminator stays observable.
    #[test]
    fn kernel_crd_schema_exposes_kind_hostpath_and_s3() {
        use kube::CustomResourceExt;
        let crd = Kernel::crd();
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
        let spec_inner = spec_props
            .properties
            .as_ref()
            .expect("spec must have properties");
        assert!(
            spec_inner.contains_key("kind"),
            "spec must have a kind discriminator"
        );
        let host_path = spec_inner
            .get("hostPath")
            .expect("spec must have a hostPath block");
        let host_path_props = host_path
            .properties
            .as_ref()
            .expect("hostPath must have properties");
        assert!(
            host_path_props.contains_key("path"),
            "hostPath must have a path field"
        );
        let s3 = spec_inner.get("s3").expect("spec must have an s3 block");
        let s3_props = s3.properties.as_ref().expect("s3 must have properties");
        for required in [
            "endpoint",
            "bucket",
            "prefix",
            "region",
            "forcePathStyle",
            "credentials",
        ] {
            assert!(
                s3_props.contains_key(required),
                "s3 must have a {required} field"
            );
        }
    }

    #[test]
    fn kernel_s3_force_path_style_round_trips_false() {
        let spec = KernelSpec {
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
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["s3"]["forcePathStyle"], false);
    }
}
