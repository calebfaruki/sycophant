fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .extern_path(".sycophant.common.v1", "::proto_common")
        .compile_protos(
            &["proto/transponder/v1/transponder.proto"],
            &["proto", "../proto-common/proto"],
        )?;
    Ok(())
}
