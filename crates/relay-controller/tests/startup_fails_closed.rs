//! The relay refuses to serve when it cannot reach the API server.
//!
//! An unsynced grants table is empty, so
//! it authorizes nobody and the registered-key load installs nothing against
//! it. Serving in that state is silent and useless — every request is refused
//! for the wrong reason, and no retry ever runs. The relay must exit instead
//! and let the kubelet's restart backoff be the retry.
//!
//! This is a binary-level test because the behavior lives in `main`'s control
//! flow around a live watcher, which no in-process harness reaches. Cargo
//! builds the binary before running integration tests and hands its path to
//! `CARGO_BIN_EXE_relay-controller`, so there is no build prerequisite and no
//! extra dev-dependency.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A kubeconfig whose server is a closed local port: client construction
/// succeeds offline, every request then fails to connect, so the watcher's
/// initial list never completes and the sync timeout fires.
const UNREACHABLE_KUBECONFIG: &str = r#"apiVersion: v1
kind: Config
clusters:
  - name: unreachable
    cluster:
      server: https://127.0.0.1:1
contexts:
  - name: unreachable
    context:
      cluster: unreachable
      user: unreachable
current-context: unreachable
users:
  - name: unreachable
    user:
      token: not-a-real-token
"#;

#[test]
fn relay_exits_nonzero_when_the_grants_watcher_never_syncs() {
    let dir = std::env::temp_dir().join(format!("syco-startup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let kubeconfig = dir.join("kubeconfig.yaml");
    std::fs::write(&kubeconfig, UNREACHABLE_KUBECONFIG).expect("write kubeconfig");

    let mut child = Command::new(env!("CARGO_BIN_EXE_relay-controller"))
        .env("KUBECONFIG", &kubeconfig)
        .env("RELAY_NAMESPACE", "startup-probe")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn relay-controller");

    // The in-process budget is 10s; allow double before calling it a hang so
    // a loaded machine does not turn a real pass into a flake.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("relay-controller still running after 20s; it must fail closed, not serve");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    assert!(
        !status.success(),
        "relay-controller exited {status}; an unreachable API server must be a startup failure"
    );

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr piped")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    assert!(
        stderr.contains("grants watcher initial sync"),
        "the exit must name the grants sync as the cause, so an operator reading \
         CrashLoopBackOff logs knows what to fix; stderr was:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
