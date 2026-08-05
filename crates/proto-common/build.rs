fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(false)
        .build_client(false)
        // The neutral message vocabulary (Message, ContentBlock, ToolCall,
        // ToolDefinition, StopReason and their block types) is the single
        // serialized form for both the on-disk conversation log and the wire
        // turn. Derive serde on every generated type so the log serializes the
        // proto types directly.
        .type_attribute(".", "#[derive(::serde::Serialize, ::serde::Deserialize)]")
        .compile_protos(&["proto/sycophant/common/v1/common.proto"], &["proto"])?;
    Ok(())
}
