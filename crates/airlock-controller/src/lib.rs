pub mod crd;
pub mod grpc;
pub mod job;
pub mod keepalive;
pub mod registry;
pub mod state;
pub mod watcher;

/// Conventional mount path for the workspace PVC inside every chamber Job.
/// Not configurable: tool images target `/workspace`.
pub const WORKSPACE_MOUNT_PATH: &str = "/workspace";
