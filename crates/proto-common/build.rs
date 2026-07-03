fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(false)
        .build_client(false)
        .compile_protos(&["proto/sycophant/common/v1/common.proto"], &["proto"])?;
    Ok(())
}
