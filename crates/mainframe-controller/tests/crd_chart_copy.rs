//! The committed CRD at `charts/sycophant-cluster/crds/kernel.yaml` drops the
//! `kind` discriminator, the `s3` sibling block, the CEL rule referencing them,
//! and the `Kind` print column, while retaining the optional host-path override.
//! A mutant that re-introduces any of the four removed elements in the generated
//! copy is caught by the matching assertion.

use std::path::PathBuf;

fn chart_crd() -> serde_yaml::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../charts/sycophant-cluster/crds/kernel.yaml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_yaml::from_str(&text).expect("chart CRD copy must be valid YAML")
}

#[test]
fn chart_crd_copy_has_no_kind_s3_cel_or_kind_column() {
    let v = chart_crd();
    let version = &v["spec"]["versions"][0];
    assert!(version.is_mapping(), "CRD must have a first version");

    if let Some(cols) = version["additionalPrinterColumns"].as_sequence() {
        for c in cols {
            assert_ne!(
                c["name"].as_str(),
                Some("Kind"),
                "chart CRD copy must not surface a `Kind` print column"
            );
            assert_ne!(
                c["jsonPath"].as_str(),
                Some(".spec.kind"),
                "chart CRD copy must not surface a print column on `.spec.kind`"
            );
        }
    }

    let schema = &version["schema"]["openAPIV3Schema"];
    if let Some(rules) = schema["x-kubernetes-validations"].as_sequence() {
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
        spec_props.is_mapping(),
        "spec must expose properties (got {spec_props:?})"
    );
    assert!(
        spec_props.get("kind").is_none(),
        "chart CRD copy must not define a `kind` discriminator"
    );
    assert!(
        spec_props.get("s3").is_none(),
        "chart CRD copy must not define an `s3` block"
    );
    // Retained: the optional host-path override with its `path` field.
    assert!(
        spec_props["hostPath"]["properties"].get("path").is_some(),
        "chart CRD copy must retain the hostPath.path override"
    );
}
