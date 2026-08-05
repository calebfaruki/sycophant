fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .extern_path(".sycophant.common.v1", "::proto_common")
        .file_descriptor_set_path(out_dir.join("toolset_descriptor.bin"))
        .compile_protos(
            &["proto/toolset/v1/toolset.proto"],
            &["proto", "../proto-common/proto"],
        )?;
    Ok(())
}
