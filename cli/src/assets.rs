use include_dir::{include_dir, Dir};

pub static CHARTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../charts/sycophant");
pub static EXAMPLES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../examples");

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
