use include_dir::{include_dir, Dir};

// Load-bearing: makes this crate depend on build.rs's chart hash so a chart edit
// forces a recompile + re-embed (include_dir! alone is invisible to cargo). Keep it.
const _: &str = include_str!(concat!(env!("OUT_DIR"), "/charts.stamp"));

pub(crate) static CLUSTER_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-cluster");
pub(crate) static TENANT_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-tenant");

/// gVisor RuntimeClass chart, installed by `syco setup` once the node runtime
/// is in place.
pub(crate) static GVISOR_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-gvisor");
/// Vendored Kyverno CRDs, installed by `syco setup` as a separate release so
/// the policy CRDs survive a Kyverno engine reinstall.
pub(crate) static KYVERNO_CRDS_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/kyverno-crds");

pub(crate) fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_cargo_pkg_version() {
        // Catches mutations replacing the function body with `""` or `"xyzzy"`.
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
        assert!(!version().is_empty());
    }
}
