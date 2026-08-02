pub mod harness {
    pub mod v1 {
        tonic::include_proto!("harness.v1");
    }
}

pub use harness::v1::*;
