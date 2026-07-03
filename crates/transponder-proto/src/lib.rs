pub mod transponder {
    pub mod v1 {
        tonic::include_proto!("transponder.v1");
    }
}

pub use transponder::v1::*;
