//! The pod pings the harness on its own. When the harness stops answering, the
//! pod stops the work of its calls well within 40 seconds of the last ping
//! answer it got, even while a command is busy printing.
//!
//! The real pod binary is started, so this test needs the pod's fixed port free
//! on this machine. A proxy sits between the harness side of the test and the
//! pod. It forwards bytes both ways until it has passed one ping answer from
//! the harness side, then goes silent in both directions without closing either
//! socket, the way a frozen or vanished harness looks to the pod.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use toolset_proto::run_tool_command::Command;
use toolset_proto::tool_job_client::ToolJobClient;
use toolset_proto::{RunToolCommand, ToolCallAssignment};

mod common;
use common::{alive, KillOnDrop};

/// The port the pod binary serves on.
const POD_PORT: u16 = 9090;

/// Kills the pod binary when the test ends.
struct PodProcess(std::process::Child);

impl Drop for PodProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Finds HTTP/2 ping answers in the bytes the harness side sends.
struct PingAnswerScanner {
    preface_left: usize,
    buf: Vec<u8>,
}

impl PingAnswerScanner {
    fn new() -> Self {
        // The client connection preface comes before the first frame.
        Self {
            preface_left: 24,
            buf: Vec::new(),
        }
    }

    /// Feed bytes; returns whether a PING frame with the ACK flag completed.
    fn feed(&mut self, bytes: &[u8]) -> bool {
        self.buf.extend_from_slice(bytes);
        let skip = self.preface_left.min(self.buf.len());
        self.buf.drain(..skip);
        self.preface_left -= skip;
        let mut answered = false;
        while self.buf.len() >= 9 {
            let len = ((self.buf[0] as usize) << 16)
                | ((self.buf[1] as usize) << 8)
                | self.buf[2] as usize;
            if self.buf.len() < 9 + len {
                break;
            }
            let (kind, flags) = (self.buf[3], self.buf[4]);
            if kind == 0x6 && flags & 0x1 == 0x1 {
                answered = true;
            }
            self.buf.drain(..9 + len);
        }
        answered
    }
}

/// Accept one connection and relay it to `pod`. Reports the moment it passed
/// the first ping answer from the harness side, then stops relaying both ways
/// while keeping both sockets open.
async fn freezing_proxy(
    listener: TcpListener,
    pod: SocketAddr,
    answered: oneshot::Sender<Instant>,
) {
    let (harness, _) = listener.accept().await.unwrap();
    let upstream = TcpStream::connect(pod).await.unwrap();
    let (mut harness_rd, mut harness_wr) = harness.into_split();
    let (mut pod_rd, mut pod_wr) = upstream.into_split();
    let frozen = CancellationToken::new();

    let freeze = frozen.clone();
    tokio::spawn(async move {
        let mut scanner = PingAnswerScanner::new();
        let mut answered = Some(answered);
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = match harness_rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if pod_wr.write_all(&buf[..n]).await.is_err() {
                break;
            }
            if scanner.feed(&buf[..n]) {
                if let Some(tx) = answered.take() {
                    let _ = tx.send(Instant::now());
                }
                freeze.cancel();
                break;
            }
        }
        // Hold both halves open and silent.
        let _keep = (harness_rd, pod_wr);
        std::future::pending::<()>().await;
    });

    tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            tokio::select! {
                _ = frozen.cancelled() => break,
                read = pod_rd.read(&mut buf) => {
                    let n = match read {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if harness_wr.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _keep = (pod_rd, harness_wr);
        std::future::pending::<()>().await;
    });
}

/// The pod runs a Shell call that prints without end. After the harness side
/// has answered at least one of the pod's pings it goes silent. The pod must
/// stop the command within 40 seconds of that last answer.
#[tokio::test]
async fn a_silent_harness_stops_the_pods_calls_within_40s_of_the_last_ping_answer() {
    let mut pod = PodProcess(
        std::process::Command::new(env!("CARGO_BIN_EXE_toolset-runtime"))
            .env("TOOLSET_TOOL_NAME", "Shell")
            .env("TOOLSET_KEEPALIVE", "true")
            .env_remove("TOOLSET_CREDENTIAL_MAP")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the pod binary starts"),
    );
    let pod_addr: SocketAddr = ([127, 0, 0, 1], POD_PORT).into();
    let ready_by = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = pod.0.try_wait().unwrap() {
            panic!("the pod exited before serving ({status}); is port {POD_PORT} already taken?");
        }
        if TcpStream::connect(pod_addr).await.is_ok() {
            break;
        }
        assert!(
            Instant::now() < ready_by,
            "the pod never served on {POD_PORT}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        pod.0.try_wait().unwrap().is_none(),
        "the pod exited; is port {POD_PORT} already taken by another process?"
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let (answered_tx, answered_rx) = oneshot::channel();
    tokio::spawn(freezing_proxy(listener, pod_addr, answered_tx));

    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let mut client = ToolJobClient::connect(format!("http://{proxy_addr}"))
        .await
        .expect("the harness side connects");
    let (outbound, rx) = mpsc::channel(4);
    outbound
        .send(RunToolCommand {
            command: Some(Command::Assignment(ToolCallAssignment {
                call_id: "printing".to_string(),
                working_dir: dir.path().to_str().unwrap().to_string(),
                args: HashMap::from([
                    (
                        "command".to_string(),
                        format!(
                            "echo $$ > '{}'; while :; do echo printing; sleep 0.05; done",
                            pidfile.display()
                        ),
                    ),
                    ("timeout".to_string(), "600".to_string()),
                ]),
            })),
        })
        .await
        .unwrap();
    let _frames = client
        .run(ReceiverStream::new(rx))
        .await
        .expect("the pod serves Run")
        .into_inner();

    let started_by = Instant::now() + Duration::from_secs(10);
    let pid = loop {
        if let Ok(s) = std::fs::read_to_string(&pidfile) {
            if s.ends_with('\n') {
                break s.trim().to_string();
            }
        }
        assert!(Instant::now() < started_by, "the command never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let _cleanup = KillOnDrop(pid.clone());

    let last_answer = tokio::time::timeout(Duration::from_secs(35), answered_rx)
        .await
        .expect("the pod never pinged the harness: no ping answer passed within 35s")
        .expect("the proxy reports the ping answer");
    assert!(
        alive(&pid),
        "the command is still printing when the harness goes silent"
    );

    let stop_by = last_answer + Duration::from_secs(40);
    while alive(&pid) {
        assert!(
            Instant::now() < stop_by,
            "the command was still running 40s after the harness's last ping answer"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
