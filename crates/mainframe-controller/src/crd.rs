use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use shared::storage::HostPathSpec;

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "sycophant.md",
    version = "v1",
    kind = "Kernel",
    shortname = "mfk",
    namespaced,
    status = "KernelStatus",
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KernelSpec {
    // Kernel content is delivered on an operator-populated read-only volume at
    // the convention path <hostPathBase>/<namespace>/<workspace>. `hostPath.path`
    // is an OPTIONAL override of the host source directory; absent → convention
    // default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_path: Option<HostPathSpec>,
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
    use kube::CustomResourceExt;

    #[test]
    fn kernel_bare_spec_round_trips() {
        // Delivery no longer has a discriminator: a Kernel with no source-shaped
        // fields is valid and round-trips to an empty spec. A mutant re-adding a
        // required `kind` field breaks this.
        let json = serde_json::json!({});
        let spec: KernelSpec = serde_json::from_value(json.clone()).unwrap();
        assert!(spec.host_path.is_none());
        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn kernel_hostpath_override_round_trips() {
        // The optional host-path override is the only source-shaped field the spec
        // retains — no `kind`, no `s3`. A mutant re-adding a required `kind` field
        // or an `s3` field breaks the exact re-serialization equality.
        let json = serde_json::json!({ "hostPath": { "path": "/Users/me/personas/web" } });
        let spec: KernelSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            spec.host_path
                .as_ref()
                .expect("hostPath override present")
                .path,
            "/Users/me/personas/web"
        );
        let re = serde_json::to_value(&spec).unwrap();
        assert_eq!(re, json);
    }

    #[test]
    fn kernel_crd_generates() {
        let crd = Kernel::crd();
        assert_eq!(crd.metadata.name.as_deref(), Some("kernels.sycophant.md"));
    }

    #[test]
    fn kernel_crd_declares_mfk_shortname() {
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

    /// The generated CRD drops the `kind` discriminator, the `s3` sibling block,
    /// the CEL rule tying `kind == S3` to a present `s3`, and the `Kind` print
    /// column on `.spec.kind`. The optional host-path override stays. Each
    /// re-introduction mutant is caught by the matching assertion.
    ///
    /// Navigates the serialized CRD (the on-disk representation) rather than the
    /// typed structs so the assertions track the generated chart copy exactly.
    #[test]
    fn kernel_crd_has_no_kind_s3_cel_or_kind_column() {
        let v = serde_json::to_value(Kernel::crd()).expect("CRD serializes");
        assert_no_kind_s3(&v);
    }

    /// Shape check over the CRD as a JSON value (camelCase keys, matching the
    /// on-disk YAML). The generated chart copy is checked with the same shape in
    /// `tests/crd_chart_copy.rs`.
    fn assert_no_kind_s3(v: &serde_json::Value) {
        let version = &v["spec"]["versions"][0];
        assert!(version.is_object(), "CRD must have a first version");

        if let Some(cols) = version["additionalPrinterColumns"].as_array() {
            for c in cols {
                assert_ne!(
                    c["name"].as_str(),
                    Some("Kind"),
                    "CRD must not surface a `Kind` print column"
                );
                assert_ne!(
                    c["jsonPath"].as_str(),
                    Some(".spec.kind"),
                    "CRD must not surface a print column on `.spec.kind`"
                );
            }
        }

        let schema = &version["schema"]["openAPIV3Schema"];
        if let Some(rules) = schema["x-kubernetes-validations"].as_array() {
            for r in rules {
                let rule = r["rule"].as_str().unwrap_or("");
                assert!(
                    !rule.contains("kind"),
                    "CEL rule must not reference kind: {rule}"
                );
                assert!(
                    !rule.contains("s3"),
                    "CEL rule must not reference s3: {rule}"
                );
            }
        }

        let spec_props = &schema["properties"]["spec"]["properties"];
        assert!(
            spec_props.is_object(),
            "spec must expose properties (got {spec_props:?})"
        );
        assert!(
            spec_props.get("kind").is_none(),
            "spec must not define a `kind` discriminator"
        );
        assert!(
            spec_props.get("s3").is_none(),
            "spec must not define an `s3` block"
        );
        // Retained: the optional host-path override with its `path` field.
        assert!(
            spec_props["hostPath"]["properties"].get("path").is_some(),
            "spec must retain the hostPath.path override"
        );
    }
}
