use include_dir::{include_dir, Dir};

// Load-bearing: makes this crate depend on build.rs's chart hash so a chart edit
// forces a recompile + re-embed (include_dir! alone is invisible to cargo). Keep it.
const _: &str = include_str!(concat!(env!("OUT_DIR"), "/charts.stamp"));

pub(crate) static CLUSTER_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-cluster");
pub(crate) static TENANT_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-tenant");
pub(crate) static AGENT_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-agent");
pub(crate) static INFERENCE_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-inference");
pub(crate) static TOOLSET_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-toolset");
pub(crate) static USER_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-user");
pub(crate) static CHANNEL_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-channel");
/// Provision, reap and suspend command templates, rendered by `sycoadm <kind> up`
/// and `down` and never installed.
pub(crate) static COMMANDS_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-commands");

/// Bundled Dex issuer, installed by `sycoadm setup`. Carries the vendored upstream
/// subchart when it is present at build time.
pub(crate) static DEX_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-dex");
/// gVisor RuntimeClass chart, installed by `sycoadm setup` once the node runtime
/// is in place.
pub(crate) static GVISOR_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-gvisor");
/// Vendored Kyverno CRDs, installed by `sycoadm setup` as a separate release so
/// the policy CRDs survive a Kyverno engine reinstall.
pub(crate) static KYVERNO_CRDS_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/kyverno-crds");
pub(crate) static OBJECTSTORE_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-objectstore");
pub(crate) static INGRESS_CHART: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant-ingress");

pub(crate) fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
