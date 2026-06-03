use kube::CustomResourceExt;
use mainframe_controller::crd::Kernel;

/// Emit the Kernel CRD's YAML to stdout for `charts/sycophant-cluster/crds/`.
///
/// Usage:
///   cargo run --example gen_crd -p mainframe-controller > charts/sycophant-cluster/crds/kernel.yaml
fn main() {
    let yaml = serde_yaml::to_string(&Kernel::crd()).unwrap();
    print!("{}", yaml);
}
