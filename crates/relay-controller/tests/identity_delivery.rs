//! The relay-identity ConfigMaps are parsed one ConfigMap at a time, and a
//! ConfigMap that fails validation is *absent*: never defaulted, never carried
//! forward from an earlier delivery, and never able to suppress the subjects
//! beside it.
//!
//! One ConfigMap is one subject: `subject`, `agents` (a comma list), and
//! `handle.<channel>` keys naming that channel's platform user ID. The table
//! answers two questions and only these: which agents a subject reaches
//! (ListAgents), and which subject a channel handle resolves to (the
//! channel path). A handle that no operator bound resolves to nothing.

use k8s_openapi::api::core::v1::ConfigMap;
use relay_controller::identity::{apply_delivery, IdentityTable};

mod common;
use common::identity_cm;

const ALICE: &str = "https://issuer.example#alice";
const BOB: &str = "https://issuer.example#bob";

fn sorted_agents(table: &IdentityTable, subject: &str) -> Vec<String> {
    let mut got = table.agents_for(subject);
    got.sort();
    got
}

/// One well-formed ConfigMap yields its subject's agent list and resolves
/// every handle it binds to that subject.
#[test]
fn a_well_formed_configmap_yields_its_subjects_list_and_handles() {
    let cms = vec![identity_cm(
        "user-0a1b2c3d4e",
        Some(ALICE),
        "beta,alpha",
        &[("slack", "U0123ABCDEF"), ("telegram", "7133824091")],
    )];
    let (table, errors) = apply_delivery(&cms);
    assert!(errors.is_empty(), "fixture must parse: {errors:?}");

    assert_eq!(
        sorted_agents(&table, ALICE),
        vec!["alpha".to_string(), "beta".to_string()],
        "the subject's whole list, straight from the watched data"
    );
    assert_eq!(table.resolve_handle("slack", "U0123ABCDEF"), Some(ALICE));
    assert_eq!(table.resolve_handle("telegram", "7133824091"), Some(ALICE));
    // Nothing else resolves: another subject, another handle.
    assert_eq!(table.resolve_handle("slack", "U999ZZZZZZZ"), None);
    assert_eq!(table.resolve_handle("email", "U0123ABCDEF"), None);
    assert!(sorted_agents(&table, BOB).is_empty());
}

/// A ConfigMap that does not carry a subject is absent, is reported by name,
/// and never suppresses the valid subject beside it.
///
/// Short-circuit the merge on the first bad ConfigMap and bob, who sits after
/// the broken one, disappears: one operator typo silently revokes an
/// unrelated person. Keep a bad one under its old values and it stays
/// reachable. Both red here.
#[test]
fn an_invalid_configmap_is_absent_and_does_not_suppress_its_neighbours() {
    let cms = vec![
        identity_cm("user-badbadbad", None, "alpha", &[]),
        identity_cm(
            "user-0a1b2c3d4e",
            Some(BOB),
            "alpha",
            &[("slack", "U0000000001")],
        ),
    ];
    let (table, errors) = apply_delivery(&cms);

    assert_eq!(
        sorted_agents(&table, BOB),
        vec!["alpha".to_string()],
        "the valid subject beside the broken one must survive"
    );
    assert!(
        table.resolve_handle("slack", "U0000000001").is_some(),
        "the valid subject's handle must survive"
    );
    assert!(
        table.agents_for("https://issuer.example#nobody").is_empty(),
        "a ConfigMap without a subject is absent, never defaulted"
    );
    assert_eq!(errors.len(), 1, "exactly the broken one is reported");
    assert_eq!(errors[0].key, "user-badbadbad");
    assert!(
        !errors[0].reason.trim().is_empty(),
        "the rejection must state a reason"
    );
}

/// A handle claimed by two subjects resolves to neither. Last-wins would
/// silently hand one person's messages to the other.
#[test]
fn a_handle_claimed_by_two_subjects_is_absent_for_both() {
    let cms = vec![
        identity_cm(
            "user-0a1b2c3d4e",
            Some(ALICE),
            "alpha",
            &[("slack", "U0123ABCDEF")],
        ),
        identity_cm(
            "user-9f8e7d6c5b",
            Some(BOB),
            "beta",
            &[("slack", "U0123ABCDEF")],
        ),
    ];
    let (table, errors) = apply_delivery(&cms);

    assert_eq!(
        table.resolve_handle("slack", "U0123ABCDEF"),
        None,
        "a doubly-claimed handle resolves to no one"
    );
    assert!(
        !errors.is_empty(),
        "the conflict must be reported so the operator can fix the binding"
    );
    let reasons = errors.iter().map(|e| e.reason.as_str()).collect::<Vec<_>>();
    assert!(
        reasons.iter().any(|r| r.contains("U0123ABCDEF")),
        "the report must name the contested handle; got {reasons:?}"
    );
    // Both subjects keep the rest of their identity.
    assert_eq!(sorted_agents(&table, ALICE), vec!["alpha".to_string()]);
    assert_eq!(sorted_agents(&table, BOB), vec!["beta".to_string()]);
}

/// A ConfigMap with a broken handle is absent as a whole: not kept by
/// skipping the broken key. Only that ConfigMap is reported.
///
/// Keeping alice's ConfigMap minus its broken handle would let her keep her
/// agents after the edit meant to cut her off.
#[test]
fn a_broken_configmap_does_not_block_a_revocation_in_the_same_delivery() {
    let cms = vec![identity_cm(
        "user-0a1b2c3d4e",
        Some(ALICE),
        "alpha",
        &[("slack", "")],
    )];
    let (table, errors) = apply_delivery(&cms);

    assert!(
        table.agents_for(ALICE).is_empty(),
        "the broken ConfigMap must be absent, not kept"
    );
    assert_eq!(table.resolve_handle("slack", "U0123ABCDEF"), None);
    assert_eq!(
        errors.len(),
        1,
        "exactly the broken one is reported: {errors:?}"
    );
    assert_eq!(errors[0].key, "user-0a1b2c3d4e");
}

/// No ConfigMaps at all is a valid delivery that revokes everyone, not an
/// error and not a reason to keep the previous table.
#[test]
fn an_empty_delivery_yields_an_empty_table_and_no_errors() {
    let empty: [ConfigMap; 0] = [];
    let (table, errors) = apply_delivery(&empty);
    assert!(errors.is_empty());
    assert!(table.agents_for(ALICE).is_empty());
    assert_eq!(table.resolve_handle("slack", "U0123ABCDEF"), None);
}
