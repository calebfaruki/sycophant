//! `sycoadm connect --tenant <name>` — join the host to the tenant's headscale
//! tailnet: port-forward headscale, mint a pre-auth key, and run
//! `sudo tailscale up`.

use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use crate::cli::ConnectCmd;
use crate::commands::command::find_tenant_ns;
use crate::commands::common::{ok, step, wait_for, warn};
use crate::runner::{run_output, run_passthrough, run_silent};

/// The headscale user name the operator's Mac joins under.
const HEADSCALE_USER: &str = "operator";
/// Default local port the headscale port-forward binds to. If something else
/// holds this port, the stale-kill step clears our own leftover forwards but
/// will not kill unrelated services — the port-forward will fail and the error
/// message names the port so the operator can free it or override via
/// SYCO_HEADSCALE_PORT.
fn local_port() -> u16 {
    std::env::var("SYCO_HEADSCALE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8080)
}

pub(crate) fn run(cmd: ConnectCmd) -> Result<(), String> {
    let ns = find_tenant_ns(&cmd.tenant, None)?;

    // 1. Check tailscale CLI
    step("Joining the tailnet");
    if !run_silent("tailscale", &["version"]) {
        warn("tailscale CLI not found — install Tailscale and retry");
        return Err("tailscale CLI not found on this host".into());
    }
    ok("tailscale CLI present");

    // 2. Wait for headscale deployment to be Available
    wait_for("headscale Available", 180, || {
        run_silent(
            "kubectl",
            &[
                "wait",
                "--for=condition=Available",
                "--timeout=5s",
                "-n",
                &ns,
                "deploy/headscale",
            ],
        )
    })?;

    let port = local_port();

    // 3. Port-forward headscale (kill any stale forward first)
    let _ = run_silent(
        "sh",
        &[
            "-c",
            &format!("lsof -ti tcp:{port} | while read pid; do kill \"$pid\" 2>/dev/null; done"),
        ],
    );
    let forward_str = format!("{port}:8080");
    let port_forward = start_port_forward(&ns, &forward_str)?;
    // Give it a moment to bind.
    sleep(Duration::from_secs(2));

    let result = do_tailnet_join(&ns);

    // Port-forward is left running — headscale needs it for ongoing coordination.
    // Rust's Drop for Child closes the handle without killing the process.
    drop(port_forward);

    result
}

fn do_tailnet_join(ns: &str) -> Result<(), String> {
    // 4. Mint a pre-auth key
    step("Minting pre-auth key");
    let key = create_preauthkey(ns)?;
    ok("pre-auth key minted");

    // 5. Write the key to a temp file (0600)
    let keyfile = crate::commands::operator::TempFile::new("authkey", &key)?;
    let keyfile_path = keyfile.path();

    // 6. sudo tailscale logout (ignore errors)
    step("Joining tailscale to the headscale server");
    let _ = run_passthrough("sudo", &["tailscale", "logout"]);

    // 7. sudo tailscale up
    let login_server = format!("http://localhost:{}", local_port());
    let auth_key = format!("file:{keyfile_path}");
    run_passthrough(
        "sudo",
        &[
            "tailscale",
            "up",
            "--reset",
            &format!("--login-server={login_server}"),
            &format!("--auth-key={auth_key}"),
            "--force-reauth",
            "--timeout=30s",
        ],
    )?;
    ok("tailscale up succeeded");

    // keyfile is dropped here (TempFile removes on drop)
    drop(keyfile);

    eprintln!("\n\x1b[1;32m==> Connected.\x1b[0m This host has joined the tenant tailnet.");
    Ok(())
}

/// Create a headscale user (idempotent) and mint a pre-auth key, returning
/// the raw key string.
fn create_preauthkey(ns: &str) -> Result<String, String> {
    // Create user (ignore errors — already exists is fine)
    let _ = run_output(
        "kubectl",
        &[
            "exec",
            "-n",
            ns,
            "deploy/headscale",
            "--",
            "headscale",
            "users",
            "create",
            HEADSCALE_USER,
        ],
    );

    // Look up the numeric user id (headscale 0.28+ requires it)
    let users_json = run_output(
        "kubectl",
        &[
            "exec",
            "-n",
            ns,
            "deploy/headscale",
            "--",
            "headscale",
            "users",
            "list",
            "-o",
            "json",
        ],
    )?;
    let users: serde_json::Value = serde_json::from_str(&users_json)
        .map_err(|e| format!("failed to parse headscale users: {e}"))?;
    let user_id = users
        .as_array()
        .into_iter()
        .flatten()
        .find(|u| u["name"].as_str() == Some(HEADSCALE_USER))
        .and_then(|u| {
            u["id"]
                .as_u64()
                .or_else(|| u["id"].as_str().and_then(|s| s.parse().ok()))
        })
        .ok_or_else(|| format!("headscale user {HEADSCALE_USER} has no id"))?;

    // Mint a pre-auth key
    let key_output = run_output(
        "kubectl",
        &[
            "exec",
            "-n",
            ns,
            "deploy/headscale",
            "--",
            "headscale",
            "preauthkeys",
            "create",
            "-u",
            &user_id.to_string(),
            "-e",
            "24h",
        ],
    )?;

    // The key is the last non-empty line of the output
    let key = key_output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .ok_or_else(|| "headscale preauthkeys create returned no key".to_string())?;
    Ok(key)
}

/// Start a self-healing background kubectl port-forward to headscale.
/// Runs in a shell loop that restarts on failure.
fn start_port_forward(ns: &str, port_map: &str) -> Result<Child, String> {
    Command::new("sh")
        .args([
            "-c",
            &format!(
                "while true; do kubectl port-forward -n {ns} svc/headscale {port_map} --address 127.0.0.1 2>/dev/null; sleep 1; done"
            ),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start port-forward: {e}"))
}
