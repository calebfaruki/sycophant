//! Acceptance test — fail-closed per-provider prompt-toolset resolution
//! (spec: "Per-provider prompt-toolset egress", AC: "When a turn's model maps
//! to a provider with no registered prompt toolset, the toolset controller
//! shall refuse the turn and shall not fall back to a default toolset or a
//! union egress allowance").
//!
//! This is the highest-value security invariant of the collapse: a spawn's
//! provider egress is pinned by which prompt toolset it runs as, so a model
//! whose provider has no `prompt-<provider-cr-name>` toolset MUST be refused,
//! never routed to some other (differently-egressing) toolset.
//!
//! Pinned contract (the coder exposes this pure resolver and calls it from the
//! Turn handler):
//!   toolset_controller::resolve_prompt_toolset(
//!       provider_ref_name: &str,
//!       registered_toolsets: &std::collections::BTreeSet<String>,
//!   ) -> Option<String>
//! Returns Some("prompt-<provider_ref_name>") iff that exact toolset is
//! registered; None (refuse the turn) otherwise. Keyed on the providerRef
//! name, never the provider format, and with no fallback.
//!
//! Materiality: fails if resolution falls back to any other registered toolset
//! when the mapped one is absent, if it keys on provider format instead of the
//! providerRef name, or if it drops the `prompt-` namespacing.

use std::collections::BTreeSet;
use toolset_controller::resolve_prompt_toolset;

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn maps_provider_to_its_own_prompt_toolset() {
    let registered = set(&["prompt-openai", "prompt-anthropic", "stdlib"]);
    assert_eq!(
        resolve_prompt_toolset("openai", &registered),
        Some("prompt-openai".to_string())
    );
}

#[test]
fn refuses_when_provider_has_no_prompt_toolset_no_fallback() {
    // A different provider's prompt toolset and an ordinary tool toolset are
    // registered. Neither may be used as a fallback: egress would leak to the
    // wrong provider (or open under the wrong CNP). Resolution must refuse.
    let registered = set(&["prompt-anthropic", "stdlib"]);
    assert_eq!(
        resolve_prompt_toolset("openai", &registered),
        None,
        "must refuse, not fall back to prompt-anthropic or stdlib"
    );
}

#[test]
fn refuses_with_no_toolsets_registered() {
    let registered = BTreeSet::new();
    assert_eq!(resolve_prompt_toolset("openai", &registered), None);
}

#[test]
fn keyed_on_provider_ref_name_not_format() {
    // Two providers of the same dialect but distinct FQDNs (hence distinct
    // CRs) must resolve to distinct prompt toolsets, so they cannot share one
    // egress CNP.
    let registered = set(&["prompt-openai-east", "prompt-openai-west"]);
    assert_eq!(
        resolve_prompt_toolset("openai-east", &registered),
        Some("prompt-openai-east".to_string())
    );
    assert_eq!(
        resolve_prompt_toolset("openai-west", &registered),
        Some("prompt-openai-west".to_string())
    );
    // A third same-format provider with no toolset of its own is still refused.
    assert_eq!(resolve_prompt_toolset("openai-north", &registered), None);
}
