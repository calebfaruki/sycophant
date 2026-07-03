pub mod sycophant {
    pub mod common {
        pub mod v1 {
            tonic::include_proto!("sycophant.common.v1");
        }
    }
}

pub use sycophant::common::v1::*;
