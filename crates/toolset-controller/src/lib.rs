//! Toolset controller: the single tenant controller that spawns credentialed
//! worker Jobs. It merges two former controllers into one gRPC service:
//!
//!   - tool dispatch over the `Toolset` CRD (spawns credentialed tool-worker
//!     Jobs that run a chamber image's tools), and
//!   - turn dispatch over the `Model`/`Provider` CRDs (spawns credentialed
//!     prompt-worker Jobs that call a model provider).
//!
//! Provider parsing lives in the prompt worker, never here — this crate does
//! not (and must not) depend on `model-provider`. A turn whose provider has no
//! registered `prompt-<provider>` toolset is refused, never routed to a
//! fallback: [`resolve_prompt_toolset`] is fail-closed by design.

pub mod audience_layer;
pub mod crd;
pub mod grpc;
pub mod job;
pub mod keepalive;
pub mod params;
pub mod registry;
pub mod state;
pub mod validation;
pub mod watcher;

/// Conventional mount path for the workspace PVC inside every tool-worker Job.
/// Not configurable: tool images target `/workspace`.
pub const WORKSPACE_MOUNT_PATH: &str = "/workspace";

/// Resolve which prompt toolset a turn's provider maps to.
///
/// Returns `Some("prompt-<provider_ref_name>")` iff that exact toolset is
/// registered, else `None` (refuse the turn). Keyed on the providerRef NAME,
/// never the provider format, and with no fallback: a model whose provider has
/// no prompt toolset of its own is refused rather than routed to some other,
/// differently-egressing toolset.
pub fn resolve_prompt_toolset(
    provider_ref_name: &str,
    registered_toolsets: &std::collections::BTreeSet<String>,
) -> Option<String> {
    let name = format!("prompt-{provider_ref_name}");
    registered_toolsets.contains(&name).then_some(name)
}
