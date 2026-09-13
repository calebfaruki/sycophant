//! Generate `BUILTIN_NAMES` from the stdlib toolset's canonical schema
//! (`images/toolset/tools.yaml`) so the runtime's builtin set and the image's
//! baked schema cannot drift. The generated slice is `include!`d by `stdlib.rs`.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // CARGO_MANIFEST_DIR = <repo>/crates/toolset-runtime
    let tools_yaml = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("crate is two levels below the repo root")
        .join("images")
        .join("toolset")
        .join("tools.yaml");

    println!("cargo:rerun-if-changed={}", tools_yaml.display());

    let raw = std::fs::read_to_string(&tools_yaml)
        .unwrap_or_else(|e| panic!("failed to read stdlib schema {}: {e}", tools_yaml.display()));
    let doc: serde_yaml::Value =
        serde_yaml::from_str(&raw).expect("stdlib tools.yaml is not valid YAML");

    // Tolerate a bare top-level list of tools or a `{tools: [...]}` mapping.
    let list = doc
        .as_sequence()
        .cloned()
        .or_else(|| doc.get("tools").and_then(|t| t.as_sequence()).cloned())
        .expect("stdlib tools.yaml is neither a list nor a {tools: [...]} mapping");

    let names: Vec<String> = list
        .iter()
        .map(|t| {
            t.get("name")
                .and_then(|n| n.as_str())
                .expect("a stdlib tool has no name")
                .to_string()
        })
        .collect();
    assert!(!names.is_empty(), "stdlib tools.yaml names no tools");

    let entries: String = names.iter().map(|n| format!("    {:?},\n", n)).collect();
    let generated = format!(
        "/// The stdlib builtins, generated at build from images/toolset/tools.yaml.\n\
         pub const BUILTIN_NAMES: &[&str] = &[\n{entries}];\n"
    );

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("builtin_names.rs");
    std::fs::write(&out, generated).expect("write builtin_names.rs");
}
