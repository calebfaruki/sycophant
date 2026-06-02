pub(crate) struct TransponderConfig {
    pub tightbeam_addr: String,
    pub airlock_addr: Option<String>,
    pub max_iterations: u32,
    pub use_stdin: bool,
}

impl TransponderConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        let tightbeam_addr = std::env::var("TIGHTBEAM_CONTROLLER_ADDR")
            .map_err(|_| "TIGHTBEAM_CONTROLLER_ADDR is required")?;

        let airlock_addr = std::env::var("AIRLOCK_CONTROLLER_ADDR").ok();

        let max_iterations = std::env::var("MAX_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        let use_stdin = std::env::var("MESSAGE_SOURCE").ok().as_deref() == Some("stdin");

        Ok(Self {
            tightbeam_addr,
            airlock_addr,
            max_iterations,
            use_stdin,
        })
    }
}
