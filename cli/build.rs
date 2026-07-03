//! Keeps the embedded Helm charts in sync with the source tree.
//!
//! `src/assets.rs` embeds `../charts` via `include_dir!`, which reads the files at
//! compile time but does NOT register them as cargo dependencies — so editing a
//! chart and rebuilding would silently ship the stale embed until `assets.rs` is
//! touched by hand. This walks the charts, tells cargo to rerun on any change, and
//! writes a content hash to `$OUT_DIR/charts.stamp`. `assets.rs` includes that
//! stamp, so a changed hash forces the crate to recompile and re-embed.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::{env, fs};

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR unset");
    let charts = Path::new(&manifest).join("../charts");
    let mut hasher = DefaultHasher::new();
    hash_dir(&charts, &mut hasher);
    let out = env::var("OUT_DIR").expect("OUT_DIR unset");
    let stamp = Path::new(&out).join("charts.stamp");
    fs::write(&stamp, format!("{:016x}", hasher.finish()))
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", stamp.display()));
}

fn hash_dir(dir: &Path, hasher: &mut DefaultHasher) {
    // Emit on the directory too, so added/deleted files trigger a rerun (a file-only
    // list misses them). Sort for a stable hash across filesystem orderings.
    println!("cargo:rerun-if-changed={}", dir.display());
    let read =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("failed to read dir {}: {e}", dir.display()));
    let mut entries: Vec<_> = read
        .map(|e| {
            e.unwrap_or_else(|e| panic!("bad dir entry in {}: {e}", dir.display()))
                .path()
        })
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            hash_dir(&path, hasher);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
            path.to_string_lossy().hash(hasher);
            fs::read(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
                .hash(hasher);
        }
    }
}
