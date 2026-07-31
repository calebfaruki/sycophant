use kube::CustomResourceExt;
use relay_controller::crd::Enrollment;

/// Emit the Enrollment CRD YAML to stdout for `charts/sycophant-cluster/crds/`.
///
/// Usage:
///   cargo run --example gen_crd > charts/sycophant-cluster/crds/enrollment.yaml
fn main() {
    print!("{}", serde_yaml::to_string(&Enrollment::crd()).unwrap());
}
