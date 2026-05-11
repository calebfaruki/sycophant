use airlock_controller::crd::Chamber;
use kube::CustomResourceExt;

fn main() {
    print!("{}", serde_yaml::to_string(&Chamber::crd()).unwrap());
}
