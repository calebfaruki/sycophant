use std::path::PathBuf;

pub(crate) struct HarnessConfig {
    pub hangar_addr: String,
    pub relay_gateway_addr: String,
    pub airlock_addr: Option<String>,
    /// Root under which this workspace's kernel directory lives. The chart
    /// mounts the read-only kernel PVC so that `<kernel_root>/<workspace>`
    /// holds AGENTS.md, agents/, and skills/.
    pub kernel_root: PathBuf,
    /// This harness's own workspace name. Each harness is per-workspace and
    /// serves only its own workspace's kernel.
    pub workspace: String,
    pub max_iterations: u32,
    pub idle_gap_secs: u64,
}

impl HarnessConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        let hangar_addr = std::env::var("HANGAR_CONTROLLER_ADDR")
            .map_err(|_| "HANGAR_CONTROLLER_ADDR is required")?;

        let relay_gateway_addr =
            std::env::var("RELAY_GATEWAY_ADDR").map_err(|_| "RELAY_GATEWAY_ADDR is required")?;

        let airlock_addr = std::env::var("AIRLOCK_CONTROLLER_ADDR").ok();

        let kernel_root = std::env::var("KERNEL_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/kernels"));

        let workspace =
            std::env::var("WORKSPACE_NAME").map_err(|_| "WORKSPACE_NAME is required")?;

        let max_iterations = std::env::var("MAX_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        // Max silence between worker events before a turn is failed as
        // wedged. Must exceed the worker heartbeat (10s) with margin.
        let idle_gap_secs = std::env::var("IDLE_GAP_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(45);

        Ok(Self {
            hangar_addr,
            relay_gateway_addr,
            airlock_addr,
            kernel_root,
            workspace,
            max_iterations,
            idle_gap_secs,
        })
    }
}
