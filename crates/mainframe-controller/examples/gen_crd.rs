use kube::CustomResourceExt;
use mainframe_controller::crd::Source;

fn main() {
    print!("{}", serde_yaml::to_string(&Source::crd()).unwrap());
}
