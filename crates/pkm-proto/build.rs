fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(out_dir.join("pkm_descriptor.bin"))
        .extern_path(".tightbeam.v1", "::tightbeam_proto")
        .compile_protos(
            &["proto/pkm/v1/pkm.proto"],
            &["proto", "../tightbeam-proto/proto"],
        )?;
    Ok(())
}
