//! Toolset controller: the single tenant controller that spawns credentialed
//! ephemeral tool jobs. It merges two former controllers into one gRPC service:
//!
//!   - tool dispatch (spawns credentialed tool Jobs that run a toolset
//!     image's tools), and
//!   - turn dispatch (spawns credentialed prompt Jobs that call a model
//!     provider).
//!
//! Tool dispatch reads the operator-authored toolset config: a flat map of
//! toolset entries, each carrying an `image`, a `keepalive`, and forwarded
//! `env` vars. Credentials and egress come from the workspace's grant menu,
//! not from the entry. Turn dispatch reads its own prompt configuration
//! section, whose profile is keyed by the call's `model` argument. An absent
//! profile key is refused, never defaulted.
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

/// Writable mount every tool Job carries so the runtime can copy a credential
/// to the convention target under the read-only root filesystem.
pub const GRANT_MOUNT_PATH: &str = "/run/secrets/grant";

/// Where a resolved grant's credential lands when the grant declares no `path`.
/// Sits under `GRANT_MOUNT_PATH`. A credential whose consumer dictates its
/// location overrides it.
pub const GRANT_CREDENTIAL_PATH: &str = "/run/secrets/grant/credential";
