pub(crate) struct TransponderConfig {
    pub tightbeam_addr: String,
    pub airlock_addr: Option<String>,
    pub mainframe_addr: String,
    pub max_iterations: u32,
}

impl TransponderConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        let tightbeam_addr = std::env::var("TIGHTBEAM_CONTROLLER_ADDR")
            .map_err(|_| "TIGHTBEAM_CONTROLLER_ADDR is required")?;

        let airlock_addr = std::env::var("AIRLOCK_CONTROLLER_ADDR").ok();
        let mainframe_addr = std::env::var("MAINFRAME_CONTROLLER_ADDR")
            .map_err(|_| "MAINFRAME_CONTROLLER_ADDR is required")?;

        let max_iterations = std::env::var("MAX_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        Ok(Self {
            tightbeam_addr,
            airlock_addr,
            mainframe_addr,
            max_iterations,
        })
    }
}
