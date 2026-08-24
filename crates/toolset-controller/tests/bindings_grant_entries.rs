//! A workspace's toolset list binds a toolset in either of two forms: a bare
//! toolset name, or a named entry carrying a grant menu. Both bind the same
//! toolset by name; only the second exposes grants.
//!
//! A grant is one operator-approved credential scoped to one (workspace,
//! toolset) pair. It names a Kubernetes Secret, and optionally a `path` where
//! the credential file lands and one `egress` domain.
//!
//! Loaded through the real `WorkspaceBindings::load` file path the controller
//! uses at startup, not an in-memory fixture.

use toolset_controller::state::WorkspaceBindings;

const MIXED_LIST: &str = "\
ws-a:
  - stdlib
  - name: notion
    grants:
      reader:
        secret: ws-a-notion-reader
        path: /home/agent/.config/notion/token
        egress: notion.com
";

/// Tests run in parallel threads and the clock is not fine-grained enough to
/// separate them, so the file name carries a counter: two tests sharing one
/// name means one loads the other's fixture.
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn write_temp(contents: &str) -> String {
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("bindings-{}-{}.yaml", std::process::id(), seq));
    std::fs::write(&path, contents).expect("write temp bindings file");
    path.to_string_lossy().into_owned()
}

fn load(contents: &str) -> Result<WorkspaceBindings, String> {
    let path = write_temp(contents);
    let result = WorkspaceBindings::load(&path);
    let _ = std::fs::remove_file(&path);
    result
}

fn load_err(contents: &str, why: &str) -> String {
    match load(contents) {
        Err(e) => e,
        Ok(_) => panic!("{why}"),
    }
}

fn loaded(contents: &str) -> WorkspaceBindings {
    load(contents).expect("the bindings file must load")
}

/// One grant menu holding a single grant named `<name>` with the given body.
fn one_grant(body: &str) -> String {
    format!("ws-a:\n  - name: notion\n    grants:\n      reader:\n{body}")
}

// ---- Both entry forms bind the same toolset by name ----

/// Breaks if the loader accepts only one entry form, or if authorization and
/// the reverse lookup compare the serialized entry shape instead of the
/// toolset name.
#[test]
fn a_bare_entry_and_a_grant_bearing_entry_both_bind_by_toolset_name() {
    let bindings = loaded(MIXED_LIST);

    assert!(
        bindings.has_toolset("ws-a", "stdlib"),
        "a bare entry binds its toolset"
    );
    assert!(
        bindings.has_toolset("ws-a", "notion"),
        "a grant-bearing entry binds by its `name`, not by its serialized map shape"
    );
    assert_eq!(
        bindings.workspaces_for_toolset("notion"),
        vec!["ws-a".to_string()],
        "the reverse lookup must find a workspace bound through a grant-bearing entry"
    );
    assert_eq!(
        bindings.workspaces_for_toolset("stdlib"),
        vec!["ws-a".to_string()],
        "the reverse lookup must still find a workspace bound through a bare entry"
    );
}

// ---- The grant menu the binding exposes ----

/// Breaks if any of a grant's three fields is dropped on load, or if the menu
/// is keyed by anything but the grant name.
#[test]
fn a_grant_bearing_entry_exposes_each_grants_secret_path_and_egress() {
    let bindings = loaded(MIXED_LIST);

    let grants = bindings
        .grants_for("ws-a", "notion")
        .expect("a grant-bearing entry exposes its menu for this workspace and toolset");
    let reader = grants
        .get("reader")
        .expect("the menu is keyed by the grant name");
    assert_eq!(reader.secret, "ws-a-notion-reader");
    assert_eq!(
        reader.path.as_deref(),
        Some("/home/agent/.config/notion/token")
    );
    assert_eq!(reader.egress.as_deref(), Some("notion.com"));
}

/// A workspace's grant menu is the closed set a call may select from. A bare
/// entry offers no set at all, which is what makes a `__grant` against it a
/// rejection rather than a lookup in an empty menu.
///
/// Breaks if a bare entry is given an empty menu instead of no menu.
#[test]
fn a_bare_entry_exposes_no_grant_menu() {
    let bindings = loaded(MIXED_LIST);

    assert!(
        bindings.grants_for("ws-a", "stdlib").is_none(),
        "a bare entry carries no menu; an empty menu is a different thing"
    );
}

/// A grant with no `egress` mounts its secret and opens nothing: the credential
/// and the network hole are separable.
///
/// Breaks if `egress` becomes required at parse.
#[test]
fn a_grant_may_declare_a_secret_and_no_egress() {
    let bindings = loaded(&one_grant(
        "        secret: ws-a-ssh-key\n        path: /home/agent/.ssh/id_ed25519\n",
    ));

    let reader = bindings
        .grants_for("ws-a", "notion")
        .expect("menu")
        .get("reader")
        .expect("grant");
    assert_eq!(reader.secret, "ws-a-ssh-key");
    assert_eq!(
        reader.egress, None,
        "a secret-only grant declares no destination and stays on the baseline floor"
    );
}

/// Breaks if `path` becomes required at parse, or if the absent case is filled
/// in at load rather than left for the job builder to default.
#[test]
fn a_grant_may_declare_a_secret_and_no_path() {
    let bindings = loaded(&one_grant(
        "        secret: ws-a-openrouter\n        egress: openrouter.ai\n",
    ));

    let reader = bindings
        .grants_for("ws-a", "notion")
        .expect("menu")
        .get("reader")
        .expect("grant");
    assert_eq!(reader.path, None, "an absent `path` stays absent at load");
}

// ---- What a grant must refuse ----

/// Breaks if `secret` becomes optional or defaultable — a grant with no Secret
/// names no credential.
#[test]
fn a_grant_with_no_secret_fails_to_load() {
    let err = load_err(
        &one_grant("        egress: notion.com\n"),
        "a grant names exactly one Secret and must not parse without it",
    );
    assert!(
        err.contains("secret"),
        "the failure must name the missing key, got: {err}"
    );
}

/// Breaks if the grant type drops `deny_unknown_fields`, letting a typo'd key
/// be silently ignored — an operator who writes `secrets:` would get a grant
/// with no credential and no warning.
#[test]
fn a_grant_with_an_unknown_key_fails_to_load() {
    let err = load_err(
        &one_grant("        secret: ws-a-notion-reader\n        secretts: extra\n"),
        "a typo'd grant key must not be silently ignored",
    );
    assert!(
        err.contains("secretts"),
        "the failure must name the unknown key, got: {err}"
    );
}

/// `path` is a mount target. A relative or empty value has no meaning as one
/// and must not be resolved into a meaning at runtime.
///
/// Breaks if the absolute-path guard is removed.
#[test]
fn a_grant_path_that_is_not_absolute_fails_to_load() {
    for path in ["", "home/agent/.ssh/id_ed25519", "../etc/passwd"] {
        let err = load_err(
            &one_grant(&format!(
                "        secret: ws-a-ssh-key\n        path: \"{path}\"\n"
            )),
            "a `path` that is not absolute is no mount target and must fail the load",
        );
        assert!(
            err.contains("path") || err.contains(path),
            "the failure must identify the offending path {path:?}, got: {err}"
        );
    }
}

/// A grant `path` must not shadow a mount the tool job already depends on: its
/// projected ServiceAccount token, the image's dispatch entrypoint, or the
/// workspace PVC. Shadowing any of them breaks the job in a way that looks like
/// an unrelated runtime fault.
///
/// Breaks if any one of the three reserved-target guards is removed.
#[test]
fn a_grant_path_naming_a_reserved_mount_fails_to_load() {
    for path in [
        "/var/run/secrets/kubernetes.io/serviceaccount",
        "/etc/toolset/dispatch",
        "/workspace",
    ] {
        let err = load_err(
            &one_grant(&format!(
                "        secret: ws-a-ssh-key\n        path: {path}\n"
            )),
            "a reserved mount target must fail the load",
        );
        assert!(
            err.contains(path) || err.contains("reserved"),
            "the failure must identify the reserved target {path}, got: {err}"
        );
    }
}
