//! Toolset controller: the single tenant controller that spawns credentialed
//! worker Jobs. It merges two former controllers into one gRPC service:
//!
//!   - tool dispatch (spawns credentialed tool-worker Jobs that run a toolset
//!     image's tools), and
//!   - turn dispatch (spawns credentialed prompt-worker Jobs that call a model
//!     provider).
//!
//! Both read the same operator-authored toolset config: a toolset entry
//! carrying an `image` and a `keepalive`, plus a map of named profiles. Tool
//! dispatch selects the profile keyed by the toolset name; turn dispatch
//! selects the profile keyed by the call's `model` argument. An absent profile
//! key is refused, never defaulted.
//!
//! Provider parsing lives in the prompt worker, never here — this crate does
//! not (and must not) depend on `model-provider`.

pub mod audience_layer;
pub mod crd;
pub mod grpc;
pub mod job;
pub mod keepalive;
pub mod registry;
pub mod state;
pub mod validation;
pub mod watcher;

/// Conventional mount path for the workspace PVC inside every tool-worker Job.
/// Not configurable: tool images target `/workspace`.
pub const WORKSPACE_MOUNT_PATH: &str = "/workspace";

/// The one parameterized toolset: its profile key is the turn's `model` value,
/// not the toolset name. Pairs to the chart-values entry key of the same name.
pub const PROMPT_TOOLSET_NAME: &str = "prompt";
