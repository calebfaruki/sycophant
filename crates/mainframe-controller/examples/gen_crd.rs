use kube::CustomResourceExt;
use mainframe_controller::crd::Kernel;
use mainframe_controller::crd::Workspace;

/// Emit a CRD's YAML to stdout for `charts/sycophant-cluster/crds/`.
///
/// Usage:
///   cargo run --example gen_crd -- kernel
///   cargo run --example gen_crd -- workspace
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let target = args.get(1).map(String::as_str).unwrap_or("kernel");
    let yaml = match target {
        "kernel" => serde_yaml::to_string(&Kernel::crd()).unwrap(),
        "workspace" => serde_yaml::to_string(&Workspace::crd()).unwrap(),
        other => {
            eprintln!("unknown CRD target: {other}; expected kernel|workspace");
            std::process::exit(1);
        }
    };
    print!("{}", yaml);
}
