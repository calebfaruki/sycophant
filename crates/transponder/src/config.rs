use std::path::PathBuf;

pub(crate) struct TransponderConfig {
    pub tightbeam_addr: String,
    pub airlock_addr: Option<String>,
    pub workspace_tools_socket: PathBuf,
    pub agent_dir: PathBuf,
    pub max_iterations: u32,
    pub use_stdin: bool,
}

impl TransponderConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        let tightbeam_addr = std::env::var("TIGHTBEAM_CONTROLLER_ADDR")
            .map_err(|_| "TIGHTBEAM_CONTROLLER_ADDR is required")?;

        let airlock_addr = std::env::var("AIRLOCK_CONTROLLER_ADDR").ok();

        let workspace_tools_socket = std::env::var("WORKSPACE_TOOLS_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/run/workspace/tools.sock"));

        let agent_dir = std::env::var("AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/agents"));

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
            workspace_tools_socket,
            agent_dir,
            max_iterations,
            use_stdin,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths() {
        // Verify the default constants used in from_env
        let default_socket = PathBuf::from("/run/workspace/tools.sock");
        let default_agents = PathBuf::from("/etc/agents");
        assert!(default_socket.is_absolute());
        assert!(default_agents.is_absolute());
    }
}
