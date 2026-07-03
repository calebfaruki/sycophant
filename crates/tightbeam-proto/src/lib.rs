pub mod tightbeam {
    pub mod v1 {
        tonic::include_proto!("tightbeam.v1");
    }
}

pub use tightbeam::v1::*;

pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("tightbeam_descriptor");
