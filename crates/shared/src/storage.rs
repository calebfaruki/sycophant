//! Storage backend specs shared between Mainframe Kernel CRDs and Hangar
//! conversation log sinks. Mainframe round-trips these from CRDs (where
//! `credentials` references a K8s Secret); Hangar constructs them in-
//! process from env vars (`credentials` is None — AWS creds flow via the
//! standard `AWS_*` env vars wired by the chart).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Absolute-path constraint for `HostPathSpec.path`. The CRD schema rejects
/// relative paths and empty strings at admission so a HostPath override always
/// names an absolute node directory.
fn absolute_path_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    serde_json::from_value(serde_json::json!({
        "type": "string",
        "pattern": "^/.+",
    }))
    .unwrap()
}

/// Local-filesystem source: a directory on the host node mounted into
/// mainframe-ctrl. Optional override of the convention path
/// `<hostPathBase>/<namespace>/<workspace>`; used by self-host operators
/// editing files in place.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostPathSpec {
    #[schemars(schema_with = "absolute_path_schema")]
    pub path: String,
}

/// S3-compatible source. `force_path_style` is required for self-hosted
/// gateways (Versitygw, MinIO, Garage) whose virtual-host-style URLs require
/// per-bucket DNS that local dev doesn't have.
///
/// `credentials` is `None` when the consumer wires AWS creds via env vars
/// (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`) instead of a K8s Secret
/// reference. Mainframe Kernels always set it; Hangar never does.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct S3Spec {
    pub endpoint: String,
    pub bucket: String,
    pub prefix: String,
    pub region: String,
    pub force_path_style: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<SecretRef>,
}

/// Reference to a Kubernetes Secret holding S3 credentials. Default keys are
/// `access-key-id` and `secret-access-key` when not specified.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_key_id_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_access_key_key: Option<String>,
}

/// Construct an `aws_sdk_s3::Client` from an `S3Spec`. AWS credentials always
/// come from the standard `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` env
/// vars (the chart wires these from the Secret named in `spec.credentials`,
/// or the operator wires them directly when `credentials` is None).
pub async fn build_s3_client(spec: &S3Spec) -> aws_sdk_s3::Client {
    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .endpoint_url(&spec.endpoint)
        .region(aws_sdk_s3::config::Region::new(spec.region.clone()))
        .load()
        .await;
    let s3_config = aws_sdk_s3::config::Builder::from(&config)
        .force_path_style(spec.force_path_style)
        .build();
    aws_sdk_s3::Client::from_conf(s3_config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_path_round_trip() {
        let json = serde_json::json!({
            "path": "/Users/me/sycophant/workspaces/foo"
        });
        let spec: HostPathSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.path, "/Users/me/sycophant/workspaces/foo");
        assert_eq!(serde_json::to_value(&spec).unwrap(), json);
    }

    #[test]
    fn s3_round_trip_full() {
        let json = serde_json::json!({
            "endpoint": "http://versitygw:7070",
            "bucket": "sycophant-tenants",
            "prefix": "tenant-abc/mainframe/",
            "region": "us-east-1",
            "forcePathStyle": true,
            "credentials": {
                "name": "tenant-s3-credentials",
                "accessKeyIdKey": "access-key-id",
                "secretAccessKeyKey": "secret-access-key"
            }
        });
        let spec: S3Spec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.endpoint, "http://versitygw:7070");
        assert_eq!(spec.bucket, "sycophant-tenants");
        assert_eq!(spec.prefix, "tenant-abc/mainframe/");
        assert!(spec.force_path_style);
        let creds = spec.credentials.as_ref().expect("credentials must be Some");
        assert_eq!(creds.name, "tenant-s3-credentials");
        assert_eq!(creds.access_key_id_key.as_deref(), Some("access-key-id"));
        assert_eq!(serde_json::to_value(&spec).unwrap(), json);
    }

    #[test]
    fn s3_round_trip_minimal_credentials() {
        let json = serde_json::json!({
            "endpoint": "http://versitygw:7070",
            "bucket": "sycophant-tenants",
            "prefix": "tenant-abc/mainframe/",
            "region": "us-east-1",
            "forcePathStyle": true,
            "credentials": { "name": "tenant-s3-credentials" }
        });
        let spec: S3Spec = serde_json::from_value(json.clone()).unwrap();
        let creds = spec.credentials.as_ref().expect("credentials must be Some");
        assert!(creds.access_key_id_key.is_none());
        assert!(creds.secret_access_key_key.is_none());
        assert_eq!(serde_json::to_value(&spec).unwrap(), json);
    }

    #[test]
    fn s3_round_trip_no_credentials() {
        let json = serde_json::json!({
            "endpoint": "http://versitygw:7070",
            "bucket": "sycophant-tenants",
            "prefix": "tenant-abc/conversations/",
            "region": "us-east-1",
            "forcePathStyle": true
        });
        let spec: S3Spec = serde_json::from_value(json.clone()).unwrap();
        assert!(spec.credentials.is_none());
        assert_eq!(serde_json::to_value(&spec).unwrap(), json);
    }

    #[test]
    fn s3_force_path_style_serializes_in_camel_case() {
        let spec = S3Spec {
            endpoint: "http://x".into(),
            bucket: "b".into(),
            prefix: "p/".into(),
            region: "us-east-1".into(),
            force_path_style: false,
            credentials: Some(SecretRef {
                name: "s".into(),
                access_key_id_key: None,
                secret_access_key_key: None,
            }),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert!(
            json.get("forcePathStyle").is_some(),
            "key must be camelCase"
        );
        assert_eq!(json["forcePathStyle"], false);
    }

    /// Kills storage.rs body of `absolute_path_schema`. Replacing the body
    /// with `Default::default()` yields an empty (permissive) schema with no
    /// `pattern`, so `HostPathSpec.path` would stop rejecting relative paths
    /// at admission. The generated JSON schema must carry the `^/.+` pattern
    /// on the `path` property.
    #[test]
    fn host_path_schema_constrains_path_to_absolute() {
        let schema = schemars::schema_for!(HostPathSpec);
        let json = serde_json::to_value(&schema).expect("schema serializes to JSON");
        assert_eq!(
            json["properties"]["path"]["pattern"],
            serde_json::json!("^/.+"),
            "path property must constrain to absolute paths"
        );
    }

    #[test]
    fn secret_ref_omits_optional_keys_when_absent() {
        let spec = SecretRef {
            name: "creds".into(),
            access_key_id_key: None,
            secret_access_key_key: None,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json, serde_json::json!({ "name": "creds" }));
    }
}
