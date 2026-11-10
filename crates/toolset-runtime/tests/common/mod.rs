//! Helpers shared by the toolset-runtime integration tests.

/// Whether `pid` is a live process. A zombie has stopped working and counts as
/// gone.
pub(crate) fn alive(pid: &str) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .expect("ps runs");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    out.status.success() && !stat.is_empty() && !stat.starts_with('Z')
}

/// Kills the process left behind if the test fails.
pub(crate) struct KillOnDrop(pub(crate) String);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = std::process::Command::new("kill")
            .args(["-9", &self.0])
            .status();
    }
}
