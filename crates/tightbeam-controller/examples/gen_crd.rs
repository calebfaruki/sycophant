use kube::CustomResourceExt;
use tightbeam_controller::crd::{Channel, Client, Model, Provider};

/// Emit a CRD's YAML to stdout for `charts/sycophant-cluster/crds/`.
///
/// Usage:
///   cargo run --example gen_crd                      # emits all four joined by ---
///   cargo run --example gen_crd -- client            # emits only Client
fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None | Some("all") => {
            print!("{}", serde_yaml::to_string(&Provider::crd()).unwrap());
            println!("---");
            print!("{}", serde_yaml::to_string(&Model::crd()).unwrap());
            println!("---");
            print!("{}", serde_yaml::to_string(&Channel::crd()).unwrap());
            println!("---");
            print!("{}", serde_yaml::to_string(&Client::crd()).unwrap());
        }
        Some("provider") => print!("{}", serde_yaml::to_string(&Provider::crd()).unwrap()),
        Some("model") => print!("{}", serde_yaml::to_string(&Model::crd()).unwrap()),
        Some("channel") => print!("{}", serde_yaml::to_string(&Channel::crd()).unwrap()),
        Some("client") => print!("{}", serde_yaml::to_string(&Client::crd()).unwrap()),
        Some(other) => {
            eprintln!("unknown CRD target: {other}; expected all|provider|model|channel|client");
            std::process::exit(1);
        }
    }
}
