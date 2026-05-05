use kube::CustomResourceExt;
use mainframe_controller::crd::Mainframe;

fn main() {
    print!("{}", serde_yaml::to_string(&Mainframe::crd()).unwrap());
}
