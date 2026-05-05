use std::path::PathBuf;

pub(crate) struct TransponderConfig {
    pub tightbeam_addr: String,
    pub airlock_addr: Option<String>,
    pub mainframe_runtime_socket: PathBuf,
    pub max_iterations: u32,
    pub use_stdin: bool,
    pub entrypoint_path: Option<PathBuf>,
}

impl TransponderConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        let tightbeam_addr = std::env::var("TIGHTBEAM_CONTROLLER_ADDR")
            .map_err(|_| "TIGHTBEAM_CONTROLLER_ADDR is required")?;

        let airlock_addr = std::env::var("AIRLOCK_CONTROLLER_ADDR").ok();

        let mainframe_runtime_socket = std::env::var("MAINFRAME_RUNTIME_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/run/mainframe/runtime.sock"));

        let max_iterations = std::env::var("MAX_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        let use_stdin = parse_use_stdin(std::env::var("MESSAGE_SOURCE").ok());

        let entrypoint_path = std::env::var("ENTRYPOINT_PATH").ok().map(PathBuf::from);

        Ok(Self {
            tightbeam_addr,
            airlock_addr,
            mainframe_runtime_socket,
            max_iterations,
            use_stdin,
            entrypoint_path,
        })
    }
}

/// Parse the `MESSAGE_SOURCE` env var into the `use_stdin` flag.
///
/// `Some("stdin")` → true; anything else → false. Separated from `from_env`
/// so the equality check is unit-testable.
fn parse_use_stdin(value: Option<String>) -> bool {
    value.map(|v| v == "stdin").unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_is_absolute() {
        let default_socket = PathBuf::from("/run/mainframe/runtime.sock");
        assert!(default_socket.is_absolute());
    }

    #[test]
    fn parse_use_stdin_recognizes_stdin() {
        assert!(parse_use_stdin(Some("stdin".to_string())));
    }

    #[test]
    fn parse_use_stdin_rejects_other_values() {
        assert!(!parse_use_stdin(Some("subscribe".to_string())));
        assert!(!parse_use_stdin(Some("".to_string())));
        assert!(!parse_use_stdin(Some("STDIN".to_string())));
    }

    #[test]
    fn parse_use_stdin_unset_is_false() {
        assert!(!parse_use_stdin(None));
    }
}
