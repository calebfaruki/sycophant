//! A toolset entry is runtime shape only. It owns no credential and no network
//! hole, so the operator-authored toolset config must refuse an entry that
//! declares either and name the offending key.
//!
//! Parsed at the depth the controller actually loads: a whole config file
//! through `ToolsetConfig::load`, not a bare inner struct.

use toolset_controller::state::ToolsetConfig;

/// Tests run in parallel threads and the clock is not fine-grained enough to
/// separate them, so the file name carries a counter: two tests sharing one
/// name means one loads the other's fixture.
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn write_temp(contents: &str) -> String {
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "toolset-config-{}-{}.yaml",
        std::process::id(),
        seq
    ));
    std::fs::write(&path, contents).expect("write temp toolset config");
    path.to_string_lossy().into_owned()
}

fn load_err(contents: &str, why: &str) -> String {
    match load(contents) {
        Err(e) => e,
        Ok(_) => panic!("{why}"),
    }
}

fn load(contents: &str) -> Result<ToolsetConfig, String> {
    let path = write_temp(contents);
    let result = ToolsetConfig::load(&path);
    let _ = std::fs::remove_file(&path);
    result
}

/// Breaks if `ToolsetEntry` keeps a `secrets` field, or loses
/// `deny_unknown_fields` so a stray key is silently dropped.
#[test]
fn an_entry_declaring_secrets_fails_to_load_and_names_the_key() {
    let err = load_err(
        "notion:\n  image: ghcr.io/x/notion:1\n  secrets:\n    - secret: notion-token\n      env: NOTION_TOKEN\n",
        "a toolset entry owns no credential, so `secrets` must not parse",
    );
    assert!(
        err.contains("secrets"),
        "the load failure must name the offending key so the operator can find it, got: {err}"
    );
}

/// Breaks if `ToolsetEntry` keeps an `egress` field, or loses
/// `deny_unknown_fields`.
#[test]
fn an_entry_declaring_egress_fails_to_load_and_names_the_key() {
    let err = load_err(
        "notion:\n  image: ghcr.io/x/notion:1\n  egress:\n    - domain: notion.com\n      port: 443\n",
        "a toolset entry opens no network hole, so `egress` must not parse",
    );
    assert!(
        err.contains("egress"),
        "the load failure must name the offending key so the operator can find it, got: {err}"
    );
}

/// The accept arm: a rejecter that rejects everything would pass the two
/// rejection tests above. Breaks if `image`, `keepalive`, or `env` is dropped
/// from the entry along with the two retired axes.
#[test]
fn an_entry_accepts_image_keepalive_and_env() {
    let config = load(
        "notion:\n  image: ghcr.io/x/notion:1\n  keepalive: true\n  env:\n    NOTION_API_VERSION: \"2022-06-28\"\n",
    )
    .expect("image, keepalive, and env are the entry's runtime shape and must parse");
    let entry = config.get("notion").expect("the entry loads under its key");
    assert_eq!(entry.image.as_deref(), Some("ghcr.io/x/notion:1"));
    assert!(entry.keepalive, "`keepalive` must survive the load");
    assert_eq!(
        entry.forwarded_env(),
        vec![("NOTION_API_VERSION".to_string(), "2022-06-28".to_string())],
        "`env` keys still forward verbatim into the tool job"
    );
}
