pub mod relay {
    pub mod v1 {
        tonic::include_proto!("relay.v1");
    }
}

pub use relay::v1::*;

pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("relay_descriptor");
