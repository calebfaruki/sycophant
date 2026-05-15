//! Storage backend specs shared between Mainframe Source CRDs and Tightbeam
//! conversation log sinks. Both kinds embed `StorageSpec` via
//! `#[serde(flatten)]` so a single discriminator (`kind`) plus sibling blocks
//! covers all backends.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Local-filesystem source: a directory on the host node mounted into the
/// workspace pod. Used by self-host operators editing files in place.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostPathSpec {
    pub path: String,
}

/// S3-compatible source. `force_path_style` is required for self-hosted
/// gateways (Versitygw, MinIO, Garage) whose virtual-host-style URLs require
/// per-bucket DNS that local dev doesn't have.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct S3Spec {
    pub endpoint: String,
    pub bucket: String,
    pub prefix: String,
    pub region: String,
    pub force_path_style: bool,
    pub credentials_secret: SecretRef,
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
            "credentialsSecret": {
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
        assert_eq!(spec.credentials_secret.name, "tenant-s3-credentials");
        assert_eq!(
            spec.credentials_secret.access_key_id_key.as_deref(),
            Some("access-key-id")
        );
        assert_eq!(serde_json::to_value(&spec).unwrap(), json);
    }

    #[test]
    fn s3_round_trip_minimal_credentials_secret() {
        let json = serde_json::json!({
            "endpoint": "http://versitygw:7070",
            "bucket": "sycophant-tenants",
            "prefix": "tenant-abc/mainframe/",
            "region": "us-east-1",
            "forcePathStyle": true,
            "credentialsSecret": { "name": "tenant-s3-credentials" }
        });
        let spec: S3Spec = serde_json::from_value(json.clone()).unwrap();
        assert!(spec.credentials_secret.access_key_id_key.is_none());
        assert!(spec.credentials_secret.secret_access_key_key.is_none());
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
            credentials_secret: SecretRef {
                name: "s".into(),
                access_key_id_key: None,
                secret_access_key_key: None,
            },
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("forcePathStyle").is_some(), "key must be camelCase");
        assert_eq!(json["forcePathStyle"], false);
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
