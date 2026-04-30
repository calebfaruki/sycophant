use std::path::PathBuf;

pub(crate) struct TransponderConfig {
    pub tightbeam_addr: String,
    pub airlock_addr: Option<String>,
    pub pkm_addr: String,
    pub workspace_tools_socket: PathBuf,
    pub max_iterations: u32,
    pub use_stdin: bool,
}

impl TransponderConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        let tightbeam_addr = std::env::var("TIGHTBEAM_CONTROLLER_ADDR")
            .map_err(|_| "TIGHTBEAM_CONTROLLER_ADDR is required")?;

        let airlock_addr = std::env::var("AIRLOCK_CONTROLLER_ADDR").ok();

        let pkm_addr =
            std::env::var("PKM_CONTROLLER_ADDR").map_err(|_| "PKM_CONTROLLER_ADDR is required")?;

        let workspace_tools_socket = std::env::var("WORKSPACE_TOOLS_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/run/workspace/tools.sock"));

        let max_iterations = std::env::var("MAX_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        let use_stdin = std::env::var("MESSAGE_SOURCE")
            .map(|v| v == "stdin")
            .unwrap_or(false);

        Ok(Self {
            tightbeam_addr,
            airlock_addr,
            pkm_addr,
            workspace_tools_socket,
            max_iterations,
            use_stdin,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_is_absolute() {
        let default_socket = PathBuf::from("/run/workspace/tools.sock");
        assert!(default_socket.is_absolute());
    }
}
