use crate::commands::setup;
use crate::runner::{run_output, run_passthrough};

/// `sycoadm destroy` — delete the sycophant k3d cluster, including every tenant and
/// all data. Irreversible. Inverse of `sycoadm setup`.
pub(crate) fn run() -> Result<(), String> {
    let list = run_output("k3d", &["cluster", "list", "-o", "json"]);
    if !plan(list)? {
        eprintln!(
            "Cluster '{}' does not exist; nothing to destroy.",
            setup::CLUSTER
        );
        return Ok(());
    }
    eprintln!(
        "Destroying the {} cluster (all tenants + data)...",
        setup::CLUSTER
    );
    run_passthrough("k3d", &["cluster", "delete", setup::CLUSTER])
}

/// Decide from `k3d cluster list -o json` whether the sycophant cluster exists
/// (true = delete). A failed or unparseable list is an error, never "absent".
fn plan(list: Result<String, String>) -> Result<bool, String> {
    let text = list.map_err(|e| format!("cannot list k3d clusters (is Docker running?): {e}"))?;
    match serde_json::from_str::<serde_json::Value>(&text) {
        // Go encodes an empty slice as null: no clusters.
        Ok(serde_json::Value::Null) => Ok(false),
        Ok(serde_json::Value::Array(clusters)) => Ok(clusters
            .iter()
            .any(|c| c.get("name").and_then(|n| n.as_str()) == Some(setup::CLUSTER))),
        _ => Err(format!(
            "unexpected output from `k3d cluster list -o json`: {text:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_refuses_when_k3d_cannot_list_clusters() {
        let err = plan(Err("k3d failed: Cannot connect to the Docker daemon".into()))
            .expect_err("a failed list must not read as 'nothing to destroy'");
        assert!(err.contains("cannot list k3d clusters"), "{err}");
        assert!(err.contains("is Docker running?"), "{err}");
        assert!(
            err.contains("k3d failed: Cannot connect to the Docker daemon"),
            "{err}"
        );
    }

    #[test]
    fn plan_refuses_unexpected_list_output() {
        // run_output returns Ok(stdout) on any exit 0, so garbage reaches plan.
        for out in ["not json", "", "{}", "1"] {
            assert!(plan(Ok(out.into())).is_err(), "{out:?}");
        }
    }

    #[test]
    fn plan_deletes_only_when_sycophant_is_listed() {
        let cases = [
            (r#"[{"name":"other"},{"name":"sycophant"}]"#, true),
            (r#"[{"name":"other"},{"name":"sycophant-dev"}]"#, false),
            ("[]", false),
            ("null", false),
        ];
        for (json, want) in cases {
            assert_eq!(plan(Ok(json.into())), Ok(want), "{json}");
        }
    }
}
