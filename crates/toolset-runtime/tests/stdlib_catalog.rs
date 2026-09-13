//! The runtime's `BUILTIN_NAMES` is derived from the stdlib toolset's checked-in
//! `tools.yaml`, not hand-maintained, so the two cannot drift.
//!
//! The stdlib toolset is the `images/toolset` image; its `tools.yaml` is the
//! single source of truth for the builtins the runtime routes, generated into
//! `BUILTIN_NAMES` at build (codegen). The load-bearing invariant this pins: the
//! set of tool names the runtime advertises as builtins is EXACTLY the set named
//! in stdlib's `tools.yaml`. It breaks if a tool is added to or removed from
//! stdlib's `tools.yaml` without flowing to `BUILTIN_NAMES` (codegen unwired or
//! the constant hand-edited).
//!
//! Offline: reads a checked-in file and a compiled-in constant. No docker.

use std::collections::BTreeSet;
use std::path::PathBuf;

use toolset_runtime::stdlib::BUILTIN_NAMES;

/// `images/toolset/tools.yaml`, the stdlib toolset's canonical schema.
fn stdlib_tools_yaml() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/toolset-runtime
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("images")
        .join("toolset")
        .join("tools.yaml")
}

/// Tool names declared in a tools.yaml, tolerating either a bare top-level list
/// of tools or a `{tools: [...]}` mapping.
fn tool_names(doc: &serde_yaml::Value) -> BTreeSet<String> {
    let list = doc
        .as_sequence()
        .cloned()
        .or_else(|| doc.get("tools").and_then(|t| t.as_sequence()).cloned())
        .unwrap_or_default();
    list.iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect()
}

#[test]
fn builtin_names_equal_stdlib_tools_yaml() {
    let path = stdlib_tools_yaml();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "stdlib codegen source {} is missing or unreadable: {e}\n\
             BUILTIN_NAMES is generated from the stdlib toolset's tools.yaml; \
             if that file moved or was deleted, the codegen source is gone.",
            path.display()
        )
    });
    let doc: serde_yaml::Value =
        serde_yaml::from_str(&raw).expect("stdlib tools.yaml is not valid YAML");

    let from_file = tool_names(&doc);
    assert!(
        !from_file.is_empty(),
        "stdlib tools.yaml names no tools; cannot be the codegen source"
    );

    let from_const: BTreeSet<String> = BUILTIN_NAMES.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        from_const, from_file,
        "BUILTIN_NAMES {:?} != stdlib tools.yaml tool names {:?}; \
         the builtin set is not derived from the single source (codegen not wired \
         or the constant was hand-edited)",
        from_const, from_file
    );
}
