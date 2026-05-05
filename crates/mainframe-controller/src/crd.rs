use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "mainframe.dev",
    version = "v1",
    kind = "Mainframe",
    namespaced,
    status = "MainframeStatus",
    printcolumn = r#"{"name":"Bucket","type":"string","jsonPath":".spec.source.s3.bucket"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"LastSync","type":"date","jsonPath":".status.lastSync"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MainframeSpec {
    pub source: MainframeSource,

    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_seconds: u64,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MainframeSource {
    pub s3: S3Source,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct S3Source {
    pub endpoint: String,
    pub bucket: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    #[serde(default = "default_region")]
    pub region: String,
    pub secret_name: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MainframeStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_count: Option<u32>,
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

fn default_refresh_interval() -> u64 {
    60
}

fn default_region() -> String {
    "us-east-1".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainframe_round_trip() {
        let json = serde_json::json!({
            "source": {
                "s3": {
                    "endpoint": "http://minio.minio.svc:9000",
                    "bucket": "sycophant-mainframe",
                    "prefix": "data/",
                    "region": "us-east-1",
                    "secretName": "mainframe-s3-creds"
                }
            },
            "refreshIntervalSeconds": 60
        });

        let spec: MainframeSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.source.s3.endpoint, "http://minio.minio.svc:9000");
        assert_eq!(spec.source.s3.bucket, "sycophant-mainframe");
        assert_eq!(spec.source.s3.prefix, "data/");
        assert_eq!(spec.source.s3.region, "us-east-1");
        assert_eq!(spec.source.s3.secret_name, "mainframe-s3-creds");
        assert_eq!(spec.refresh_interval_seconds, 60);

        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn mainframe_defaults() {
        let json = serde_json::json!({
            "source": {
                "s3": {
                    "endpoint": "http://minio.minio.svc:9000",
                    "bucket": "sycophant-mainframe",
                    "secretName": "creds"
                }
            }
        });

        let spec: MainframeSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec.refresh_interval_seconds, 60);
        assert_eq!(spec.source.s3.region, "us-east-1");
        assert_eq!(spec.source.s3.prefix, "");
    }

    #[test]
    fn mainframe_crd_generates() {
        use kube::CustomResourceExt;
        let crd = Mainframe::crd();
        assert_eq!(
            crd.metadata.name.as_deref(),
            Some("mainframes.mainframe.dev")
        );
    }
}
