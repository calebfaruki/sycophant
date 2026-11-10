//! A tool call's work lives only as long as its stream to the harness. When the
//! stream ends, however it ends, the pod stops that call's work and leaves the
//! pod's other calls running.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use shared::scrub::ScrubSet;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Channel, Server};

use toolset_proto::run_tool_command::Command;
use toolset_proto::tool_job_client::ToolJobClient;
use toolset_proto::tool_job_server::ToolJobServer;
use toolset_proto::{RunToolCommand, ToolCallAssignment};
use toolset_runtime::execute::stream_frames;

mod common;
use common::{alive, KillOnDrop};

/// A named pipe the tool under test reads. The test holds the write end, so
/// the reader never sees end of file while it lives, and a write to it fails
/// with a broken pipe once no process has the pipe open for reading.
struct Fifo {
    path: PathBuf,
    opened: std::sync::mpsc::Receiver<File>,
    writer: Option<File>,
}

impl Fifo {
    fn new(dir: &Path, name: &str) -> Self {
        let path = dir.join(name);
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo runs");
        assert!(status.success(), "mkfifo {}", path.display());
        // A write-only open blocks until a reader opens the pipe, so it runs on
        // its own thread and hands the file back once the tool has opened it.
        let (tx, opened) = std::sync::mpsc::channel();
        let open_path = path.clone();
        std::thread::spawn(move || {
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(open_path) {
                let _ = tx.send(f);
            }
        });
        Self {
            path,
            opened,
            writer: None,
        }
    }

    fn path(&self) -> String {
        self.path.to_str().unwrap().to_string()
    }

    /// Wait until the tool has opened the pipe for reading.
    fn wait_opened(&mut self, what: &str) {
        let f = self
            .opened
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("{what}: the tool never opened its pipe"));
        self.writer = Some(f);
    }

    /// Whether some process still has the pipe open for reading.
    fn has_reader(&mut self) -> bool {
        match self.writer.as_mut().expect("opened").write(b"a") {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => false,
            Err(e) => panic!("probe write to {}: {e}", self.path.display()),
        }
    }
}

/// Serve `tool` as a pod would, on an ephemeral port, and return its endpoint.
async fn serve(tool: &str) -> String {
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reserve.local_addr().unwrap();
    drop(reserve);
    let service = toolset_runtime::ToolJobService::new(tool);
    tokio::spawn(async move {
        Server::builder()
            .add_service(ToolJobServer::new(service))
            .serve(addr)
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

/// One call held open from the harness side: its own connection, its
/// outbound half and its response half. Dropping it ends the call's stream.
struct HeldCall {
    _client: ToolJobClient<Channel>,
    _outbound: mpsc::Sender<RunToolCommand>,
    _frames: tonic::Streaming<proto_common::ToolResultFrame>,
}

async fn start_call(
    endpoint: &str,
    call_id: &str,
    dir: &Path,
    args: HashMap<String, String>,
) -> HeldCall {
    let mut client = loop {
        match ToolJobClient::connect(endpoint.to_string()).await {
            Ok(c) => break c,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };
    let (outbound, rx) = mpsc::channel(4);
    outbound
        .send(RunToolCommand {
            command: Some(Command::Assignment(ToolCallAssignment {
                call_id: call_id.to_string(),
                working_dir: dir.to_str().unwrap().to_string(),
                args,
            })),
        })
        .await
        .unwrap();
    let frames = client
        .run(ReceiverStream::new(rx))
        .await
        .expect("the pod serves Run")
        .into_inner();
    HeldCall {
        _client: client,
        _outbound: outbound,
        _frames: frames,
    }
}

fn shell_args(fifo: &str) -> HashMap<String, String> {
    HashMap::from([("command".to_string(), format!("exec cat -- '{fifo}'"))])
}

fn search_args(fifo: &str) -> HashMap<String, String> {
    HashMap::from([
        ("target".to_string(), "content".to_string()),
        ("pattern".to_string(), "needle".to_string()),
        ("path".to_string(), fifo.to_string()),
    ])
}

/// Two calls of the same built-in tool run on one pod, each blocked reading
/// its own pipe. Ending the first call's stream stops the first call's work:
/// its process lets go of the pipe. The second call keeps running. Checked for
/// Shell and for Search.
#[tokio::test]
async fn a_closed_call_stream_stops_only_that_calls_builtin_work() {
    type ArgsFor = fn(&str) -> HashMap<String, String>;
    let cases: [(&str, ArgsFor); 2] = [("Shell", shell_args), ("Search", search_args)];
    for (tool, args) in cases {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = serve(tool).await;

        let mut closed_fifo = Fifo::new(dir.path(), "closed.fifo");
        let mut kept_fifo = Fifo::new(dir.path(), "kept.fifo");
        let closed = start_call(&endpoint, "closed", dir.path(), args(&closed_fifo.path())).await;
        let kept = start_call(&endpoint, "kept", dir.path(), args(&kept_fifo.path())).await;
        closed_fifo.wait_opened(&format!("{tool} closed call"));
        kept_fifo.wait_opened(&format!("{tool} kept call"));
        assert!(
            closed_fifo.has_reader(),
            "{tool}: the first call is running"
        );
        assert!(kept_fifo.has_reader(), "{tool}: the second call is running");

        drop(closed);

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while closed_fifo.has_reader() {
            assert!(
                std::time::Instant::now() < deadline,
                "{tool}: the call whose stream ended is still running"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            kept_fifo.has_reader(),
            "{tool}: the pod's other call must keep running"
        );

        // End of file on the kept pipe lets the second call finish on its own.
        drop(kept_fifo);
        drop(kept);
    }
}

/// A command keeps printing and does not die when its output pipe closes.
/// Once the receiving end of its frame stream is gone, the producer returns
/// and the command must be dead too, with no cancel fired.
#[tokio::test]
async fn a_printing_command_dies_when_its_output_stream_closes() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(format!(
        "trap '' PIPE; echo $$ > '{}'; for i in $(seq 1 3000); do echo line $i; sleep 0.01; done",
        pidfile.display()
    ));

    let (tx, mut rx) = mpsc::channel(1);
    let producer = tokio::spawn(async move {
        let cancel = CancellationToken::new();
        let scrub = ScrubSet::from_env_var("__UNSET_CLOSED_STREAM_TEST_SCRUB__");
        stream_frames(cmd, &cancel, None, &scrub, tx).await;
    });

    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the command prints")
        .expect("a frame");
    let pid = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .to_string();
    let _cleanup = KillOnDrop(pid.clone());
    assert!(
        alive(&pid),
        "the command is running before its stream closes"
    );

    drop(rx);
    tokio::time::timeout(Duration::from_secs(5), producer)
        .await
        .expect("the producer returns once its output stream is closed")
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while alive(&pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "the command kept running after its output stream closed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
