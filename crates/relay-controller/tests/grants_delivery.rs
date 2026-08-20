//! The grants ConfigMap is parsed one row at a time, and a row that fails
//! validation is *absent*: never defaulted, never carried forward from an
//! earlier delivery, and never able to suppress the rows beside it.
//!
//! The contract these tests pin:
//!
//! ```ignore
//! pub struct GrantRow { pub channel: String, pub identity: String, pub workspace: String }
//! pub struct RowError { pub key: String, pub reason: String }
//! pub struct GrantsTable;
//! impl GrantsTable {
//!     pub fn get(&self, row_key: &str) -> Option<&GrantRow>;
//!     pub fn len(&self) -> usize;
//!     pub fn is_empty(&self) -> bool;
//! }
//! pub fn parse_row(key: &str, yaml: &str) -> Result<GrantRow, RowError>;
//! pub fn apply_delivery(cm: &ConfigMap) -> (GrantsTable, Vec<RowError>);
//! ```

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use relay_controller::grants::{apply_delivery, parse_row, GrantsTable};

const GRANTS_CONFIGMAP: &str = "grants";

fn delivery(rows: &[(&str, &str)]) -> ConfigMap {
    let mut data = BTreeMap::new();
    for (key, body) in rows {
        data.insert((*key).to_string(), (*body).to_string());
    }
    ConfigMap {
        metadata: ObjectMeta {
            name: Some(GRANTS_CONFIGMAP.into()),
            namespace: Some("tenant".into()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    }
}

fn row_yaml(channel: &str, identity: &str, workspace: &str) -> String {
    format!("channel: {channel}\nidentity: {identity}\nworkspace: {workspace}\n")
}

fn assert_row(table: &GrantsTable, key: &str, channel: &str, identity: &str, workspace: &str) {
    let row = table
        .get(key)
        .unwrap_or_else(|| panic!("row {key} must be present in the table"));
    assert_eq!(row.channel, channel, "row {key} channel");
    assert_eq!(row.identity, identity, "row {key} identity");
    assert_eq!(row.workspace, workspace, "row {key} workspace");
}

/// One delivery, five rows, three of them broken in the three ways a row can be
/// broken. Every good row lands; every bad row is absent; every bad row is
/// reported by key with a reason.
///
/// Make `apply_delivery` short-circuit on the first `Err` and `caleb-email`, which sits
/// after the broken rows in key order, disappears. One operator typo would then
/// silently revoke an unrelated person's access.
#[test]
fn one_bad_row_does_not_take_the_good_rows_with_it() {
    let cm = delivery(&[
        ("bad-channel", &row_yaml("carrier-pigeon", "abc", "family")),
        ("bad-identity", &row_yaml("app", "", "family")),
        ("bad-workspace", &row_yaml("app", "kJ8f2QwXnR4tYv6b", "")),
        (
            "caleb-phone",
            &row_yaml("app", "kJ8f2QwXnR4tYv6b", "family"),
        ),
        (
            "caleb-email",
            &row_yaml("email", "calebfaruki@hey.com", "family"),
        ),
    ]);

    let (table, errors) = apply_delivery(&cm);

    assert_eq!(table.len(), 2, "exactly the two well-formed rows");
    assert_row(&table, "caleb-phone", "app", "kJ8f2QwXnR4tYv6b", "family");
    assert_row(
        &table,
        "caleb-email",
        "email",
        "calebfaruki@hey.com",
        "family",
    );

    assert!(
        table.get("bad-channel").is_none(),
        "unknown channel is absent"
    );
    assert!(
        table.get("bad-identity").is_none(),
        "empty identity is absent"
    );
    assert!(
        table.get("bad-workspace").is_none(),
        "empty workspace is absent"
    );

    // The Warning Event's payload is built from these: the row key and the
    // reason are what `kubectl describe` has to surface, so both must survive
    // validation.
    let mut reported: Vec<&str> = errors.iter().map(|e| e.key.as_str()).collect();
    reported.sort_unstable();
    assert_eq!(
        reported,
        vec!["bad-channel", "bad-identity", "bad-workspace"]
    );
    for err in &errors {
        assert!(
            !err.reason.trim().is_empty(),
            "row {} was rejected with no stated reason",
            err.key
        );
    }
}

/// A bad row must never block a revocation made in the same edit. The operator
/// deletes `dad-telegram` and, in the same `kubectl edit`, fat-fingers
/// `caleb-phone` into unparseable YAML.
///
/// Any last-known-good, blue-green, or "keep the previous map when this one has
/// errors" strategy keeps `dad-telegram` alive here, and that is invisible to a
/// test that only ever feeds one delivery.
#[test]
fn a_malformed_row_does_not_block_a_revocation_in_the_same_delivery() {
    let first = delivery(&[
        (
            "caleb-phone",
            &row_yaml("app", "kJ8f2QwXnR4tYv6b", "family"),
        ),
        (
            "dad-telegram",
            &row_yaml("telegram", "7133824091", "family"),
        ),
    ]);
    let (table, errors) = apply_delivery(&first);
    assert!(errors.is_empty(), "first delivery is clean");
    assert_eq!(table.len(), 2);

    let second = delivery(&[("caleb-phone", "channel: app\n  identity: [oops\n")]);
    let (table, errors) = apply_delivery(&second);

    assert!(
        table.get("dad-telegram").is_none(),
        "the removed row must be gone even though the surviving row failed to parse"
    );
    assert!(
        table.get("caleb-phone").is_none(),
        "the malformed row must not fall back to its previous value"
    );
    assert!(table.is_empty(), "no row survives this delivery");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].key, "caleb-phone");
}

/// A delivery that simply drops a row must drop it. This is the ordinary
/// revocation path and the only place a cached map can leak back in.
///
/// Hold the table in a map that is updated rather than replaced,
/// `for (k, v) in new { table.insert(k, v) }`, and `dad-telegram` survives its
/// own deletion forever.
#[test]
fn a_delivery_that_omits_a_row_revokes_it() {
    let (before, _) = apply_delivery(&delivery(&[
        (
            "caleb-phone",
            &row_yaml("app", "kJ8f2QwXnR4tYv6b", "family"),
        ),
        (
            "dad-telegram",
            &row_yaml("telegram", "7133824091", "family"),
        ),
    ]));
    assert!(before.get("dad-telegram").is_some());

    let (after, errors) = apply_delivery(&delivery(&[(
        "caleb-phone",
        &row_yaml("app", "kJ8f2QwXnR4tYv6b", "family"),
    )]));

    assert!(errors.is_empty());
    assert_eq!(after.len(), 1);
    assert!(after.get("dad-telegram").is_none());
}

/// An empty `data` block is a valid delivery that revokes everything, not an
/// error and not a reason to keep the previous table.
///
/// Treat "no data" as "nothing changed" and `helm uninstall`-shaped
/// data loss stops revoking anyone.
#[test]
fn a_delivery_with_no_rows_yields_an_empty_table_and_no_errors() {
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(GRANTS_CONFIGMAP.into()),
            namespace: Some("tenant".into()),
            ..Default::default()
        },
        data: None,
        ..Default::default()
    };
    let (table, errors) = apply_delivery(&cm);
    assert!(table.is_empty());
    assert!(errors.is_empty());
}

/// The grant row's schema is exactly `channel`, `identity`, `workspace`.
/// A row carrying a scope, capability, or profile field is not a row with an
/// extra hint; it is a row the relay does not understand, so it is absent.
///
/// Parse leniently (serde's default: ignore unknown fields) and a
/// scope field can be introduced by an operator, or later by a contributor,
/// with the relay silently granting the full conversational surface regardless.
/// "Fail closed, no defaults" is what forbids that, and this is the only test
/// that observes it.
#[test]
fn a_row_carrying_a_scope_field_is_rejected() {
    let err = parse_row(
        "caleb-phone",
        "channel: app\nidentity: kJ8f2QwXnR4tYv6b\nworkspace: family\nscope: read-only\n",
    )
    .expect_err("a row with a scope field must not parse");
    assert_eq!(err.key, "caleb-phone");
    assert!(
        err.reason.contains("scope"),
        "the reason must name the offending field, got: {}",
        err.reason
    );
}

/// Each row parses independently of the others, so `parse_row` is reachable and
/// total on one row's text.
///
/// Fold row parsing into a whole-map deserialize and a single bad row becomes a
/// whole-map error, which would let one typo revoke everyone. This test pins
/// that the per-row seam exists at all.
#[test]
fn parse_row_reads_one_row_without_seeing_the_map() {
    let row = parse_row(
        "dad-telegram",
        &row_yaml("telegram", "7133824091", "family"),
    )
    .expect("a well-formed row parses on its own");
    assert_eq!(row.channel, "telegram");
    assert_eq!(row.identity, "7133824091");
    assert_eq!(row.workspace, "family");
}
