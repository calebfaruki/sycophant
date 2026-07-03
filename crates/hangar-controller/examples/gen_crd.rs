use hangar_controller::crd::{Model, Provider};
use kube::CustomResourceExt;

/// Emit a CRD's YAML to stdout for `charts/sycophant-cluster/crds/`.
///
/// Usage:
///   cargo run --example gen_crd                      # emits both joined by ---
///   cargo run --example gen_crd -- model             # emits only Model
fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None | Some("all") => {
            print!("{}", serde_yaml::to_string(&Provider::crd()).unwrap());
            println!("---");
            print!("{}", serde_yaml::to_string(&Model::crd()).unwrap());
        }
        Some("provider") => print!("{}", serde_yaml::to_string(&Provider::crd()).unwrap()),
        Some("model") => print!("{}", serde_yaml::to_string(&Model::crd()).unwrap()),
        Some(other) => {
            eprintln!("unknown CRD target: {other}; expected all|provider|model");
            std::process::exit(1);
        }
    }
}
