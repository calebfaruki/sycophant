use crate::commands::setup;
use crate::runner::{run_output, run_passthrough};

/// `syco destroy` — delete the sycophant k3d cluster, including every tenant and
/// all data. Irreversible. Inverse of `syco setup`.
pub(crate) fn run() -> Result<(), String> {
    let list = run_output("k3d", &["cluster", "list", "-o", "json"]).unwrap_or_default();
    if !setup::cluster_exists(&list, "sycophant") {
        eprintln!("Cluster 'sycophant' does not exist; nothing to destroy.");
        return Ok(());
    }
    eprintln!("Destroying the sycophant cluster (all tenants + data)...");
    run_passthrough("k3d", &["cluster", "delete", "sycophant"])
}
