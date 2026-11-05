//! The relay refuses to serve when it cannot reach the API server.
//!
//! An unsynced identity table is empty, so it lists no agents and no
//! handle resolves. Serving in that state is silent and useless — callers are
//! refused for the wrong reason, and no retry ever runs. The relay must exit
//! instead and let the kubelet's restart backoff be the retry.
//!
//! This is a binary-level test because the behavior lives in `main`'s control
//! flow around a live watcher, which no in-process harness reaches. Cargo
//! builds the binary before running integration tests and hands its path to
//! `CARGO_BIN_EXE_relay-controller`, so there is no build prerequisite and no
//! extra dev-dependency.
//!
//! With an unreachable apiserver every awaited watcher times out, and the
//! first one's timeout names the exit. A second test isolates the identity
//! watcher: the apiserver lists the toolset-grants ConfigMaps but never the
//! identity ones.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A kubeconfig whose server is a closed local port: client construction
/// succeeds offline, every request then fails to connect, so every watcher's
/// initial list never completes and a sync timeout fires.
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
fn relay_exits_nonzero_when_a_watcher_never_syncs() {
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
        match child.try_wait().expect("try wait") {
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

    // The exit must name a watcher initial sync as the cause, so an operator
    // reading CrashLoopBackOff logs knows what to fix.
    assert!(
        stderr.contains("watcher initial sync"),
        "the exit must name a watcher sync as the cause; stderr was:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Wait up to 20s for `child` to exit. The in-process sync budget is 10s;
/// double it before calling it a hang so a loaded machine does not turn a
/// real pass into a flake.
fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait().expect("try wait") {
            Some(status) => return status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("relay-controller still running after 20s; it must fail closed, not serve");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Read one request head off `conn`; `None` once the client hangs up.
fn read_request_line(reader: &mut BufReader<TcpStream>) -> Option<String> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None;
    }
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        if header == "\r\n" {
            return Some(request_line);
        }
    }
}

/// Serve one client connection: answer a toolset-grants list with an empty
/// ConfigMapList, and hold every other request (the identity list, and the
/// toolset watch that follows its list) open without answering.
fn serve_connection(conn: TcpStream) {
    let mut writer = conn.try_clone().expect("clone connection");
    let mut reader = BufReader::new(conn);
    while let Some(request_line) = read_request_line(&mut reader) {
        let is_toolset_list =
            request_line.contains("relay-toolset-grants") && !request_line.contains("watch=");
        if !is_toolset_list {
            // Hold until the relay hangs up.
            let mut sink = Vec::new();
            let _ = reader.read_to_end(&mut sink);
            return;
        }
        let body = r#"{"apiVersion":"v1","kind":"ConfigMapList","metadata":{"resourceVersion":"1"},"items":[]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        if writer.write_all(response.as_bytes()).is_err() {
            return;
        }
    }
}

/// The identity table alone failing to sync is fatal. The toolset-grants
/// watcher syncs, so only the identity watcher's timeout can end the process.
///
/// Ignoring the identity watcher's sync result in `main` would let the
/// relay serve an empty identity table.
#[test]
fn relay_exits_nonzero_when_the_identity_watcher_never_syncs() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock apiserver");
    let addr = listener.local_addr().expect("mock apiserver addr");
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            std::thread::spawn(move || serve_connection(conn));
        }
    });

    let dir = std::env::temp_dir().join(format!("syco-startup-identity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let kubeconfig = dir.join("kubeconfig.yaml");
    std::fs::write(
        &kubeconfig,
        UNREACHABLE_KUBECONFIG.replace("https://127.0.0.1:1", &format!("http://{addr}")),
    )
    .expect("write kubeconfig");

    let mut child = Command::new(env!("CARGO_BIN_EXE_relay-controller"))
        .env("KUBECONFIG", &kubeconfig)
        .env("RELAY_NAMESPACE", "startup-probe")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn relay-controller");

    let status = wait_for_exit(&mut child);
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout piped")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr piped")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    assert!(
        stdout.contains("toolset-grants watcher initial sync complete"),
        "the toolset-grants watcher must sync, isolating the identity watcher; \
         stdout was:\n{stdout}"
    );
    assert!(
        !status.success(),
        "relay-controller exited {status}; an unsynced identity table must be a startup failure"
    );
    assert!(
        stderr.contains("identity watcher initial sync"),
        "the exit must name the identity sync as the cause; stderr was:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
