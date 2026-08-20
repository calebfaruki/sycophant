//! Toolset controller: the single tenant controller that spawns credentialed
//! ephemeral tool jobs. It merges two former controllers into one gRPC service:
//!
//!   - tool dispatch (spawns credentialed tool Jobs that run a toolset
//!     image's tools), and
//!   - turn dispatch (spawns credentialed prompt Jobs that call a model
//!     provider).
//!
//! Tool dispatch reads the operator-authored toolset config: a flat map of
//! toolset entries, each carrying an `image`, a `keepalive`, `secrets`,
//! `egress`, and forwarded `env` vars. Turn dispatch reads its own prompt
//! configuration section, whose profile is keyed by the call's `model`
//! argument. An absent profile key is refused, never defaulted.
//!
//! Provider parsing lives in the prompt job, never here — this crate does
//! not (and must not) depend on `model-provider`.

pub mod audience_layer;
pub mod config;
pub mod grpc;
pub mod job;
pub mod keepalive;
pub mod registry;
pub mod state;
pub mod validation;
pub mod watcher;

/// Conventional mount path for the workspace PVC inside every tool Job.
/// Not configurable: tool images target `/workspace`.
pub const WORKSPACE_MOUNT_PATH: &str = "/workspace";
