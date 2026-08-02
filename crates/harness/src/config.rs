pub(crate) struct HarnessConfig {
    pub hangar_addr: String,
    pub relay_gateway_addr: String,
    pub airlock_addr: Option<String>,
    pub mainframe_addr: String,
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
        let mainframe_addr = std::env::var("MAINFRAME_CONTROLLER_ADDR")
            .map_err(|_| "MAINFRAME_CONTROLLER_ADDR is required")?;

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
            mainframe_addr,
            max_iterations,
            idle_gap_secs,
        })
    }
}
