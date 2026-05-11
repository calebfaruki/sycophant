use kube::CustomResourceExt;
use tightbeam_controller::crd::{Channel, Model, Provider};

fn main() {
    print!("{}", serde_yaml::to_string(&Provider::crd()).unwrap());
    println!("---");
    print!("{}", serde_yaml::to_string(&Model::crd()).unwrap());
    println!("---");
    print!("{}", serde_yaml::to_string(&Channel::crd()).unwrap());
}
