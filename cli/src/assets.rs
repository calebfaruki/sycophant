use include_dir::{include_dir, Dir};

pub(crate) static CLUSTER_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-cluster");
pub(crate) static TENANT_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-tenant");
pub(crate) static EXAMPLES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../examples");

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
