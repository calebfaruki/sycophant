use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "sycophant.md",
    version = "v1",
    kind = "Enrollment",
    shortname = "enr",
    namespaced,
    status = "EnrollmentStatus",
    printcolumn = r#"{"name":"Workspaces","type":"string","jsonPath":".spec.workspaces"}"#,
    printcolumn = r#"{"name":"Enrolled","type":"string","jsonPath":".status.enrolledAt"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentSpec {
    /// Bare-string references to the Workspaces this enrollment is
    /// authorized to act on. Same convention as Workspace.spec.kernels
    /// — references live in the same namespace, the field name implies
    /// the kind.
    pub workspaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentStatus {
    /// One-time enrollment code minted by the controller when the
    /// Enrollment has no registered public key. JWT carrying
    /// {workspace, device_name (= Enrollment CR name), code_id, exp}.
    /// Cleared on successful RedeemEnrollment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_code: Option<String>,
    /// Unix-seconds expiry of the current enrollmentCode, for
    /// operator visibility. Cleared alongside enrollmentCode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_code_expires_at: Option<i64>,
    /// Base64-encoded SEC1 P-256 public key registered via
    /// RedeemEnrollment. Re-enrollment (operator patches this to
    /// null via the status subresource) causes the controller to
    /// mint a fresh enrollmentCode on the next reconcile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    /// RFC 3339 timestamp of the most recent successful enrollment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrolled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<EnrollmentCondition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentCondition {
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
    use kube::{CustomResourceExt, Resource};

    #[test]
    fn enrollment_crd_generates_correct_kind() {
        assert_eq!(Enrollment::kind(&()), "Enrollment");
        assert_eq!(Enrollment::group(&()), "sycophant.md");
        assert_eq!(Enrollment::version(&()), "v1");
    }

    #[test]
    fn enrollment_spec_serializes_camel_case() {
        let spec = EnrollmentSpec {
            workspaces: vec!["hello-world".into()],
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"workspaces\":[\"hello-world\"]"));
    }

    #[test]
    fn enrollment_status_round_trips_camel_case_fields() {
        let status = EnrollmentStatus {
            enrollment_code: Some("jwt".into()),
            enrollment_code_expires_at: Some(1_700_000_000),
            public_key: Some("pk-b64".into()),
            enrolled_at: Some("2026-06-29T00:00:00Z".into()),
            conditions: vec![],
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"enrollmentCode\":\"jwt\""));
        assert!(json.contains("\"enrollmentCodeExpiresAt\":1700000000"));
        assert!(json.contains("\"publicKey\":\"pk-b64\""));
        assert!(json.contains("\"enrolledAt\":\"2026-06-29T00:00:00Z\""));
        let back: EnrollmentStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back.public_key.as_deref(), Some("pk-b64"));
    }

    #[test]
    fn enrollment_status_omits_none_fields() {
        // SSA clears by omission; a default status must serialize without
        // the optional keys so a patch built from it does not re-assert them.
        let json = serde_json::to_string(&EnrollmentStatus::default()).unwrap();
        assert!(!json.contains("enrollmentCode"));
        assert!(!json.contains("publicKey"));
        assert!(!json.contains("enrolledAt"));
    }

    #[test]
    fn enrollment_crd_declares_status_subresource() {
        let crd = Enrollment::crd();
        let version = crd
            .spec
            .versions
            .first()
            .expect("CRD must declare at least one version");
        assert!(
            version
                .subresources
                .as_ref()
                .is_some_and(|s| s.status.is_some()),
            "Enrollment must declare a status subresource so SSA status patches work"
        );
    }
}
