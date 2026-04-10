use kube::CustomResourceExt;
use tightbeam_controller::crd::{TightbeamChannel, TightbeamModel};

fn main() {
    print!("{}", serde_yaml::to_string(&TightbeamModel::crd()).unwrap());
    println!("---");
    print!(
        "{}",
        serde_yaml::to_string(&TightbeamChannel::crd()).unwrap()
    );
}
