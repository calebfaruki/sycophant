use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use proto_common::ToolResultFrame;
use shared::scrub::ScrubSet;
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status, Streaming};

use toolset_proto::run_tool_command::Command;
use toolset_proto::tool_job_server::ToolJob;
use toolset_proto::RunToolCommand;

use crate::{execute, parts, stdlib};

/// Bound on a served call's outbound frame channel, matching the harness's
/// inbound bound.
const RESULT_CHANNEL_CAPACITY: usize = 64;

/// Run activity the shutdown watcher reads: how many calls are executing and
/// when the last one started or finished. A fresh process counts its own start
/// as the last activity so an idle keepalive pod that is never dialed still
/// exits after its window.
struct Activity {
    in_flight: AtomicI64,
    completed: AtomicI64,
    last_activity: Mutex<Instant>,
}

impl Activity {
    fn new() -> Self {
        Self {
            in_flight: AtomicI64::new(0),
            completed: AtomicI64::new(0),
            last_activity: Mutex::new(Instant::now()),
        }
    }

    async fn touch(&self) {
        *self.last_activity.lock().await = Instant::now();
    }
}

/// The gRPC server the harness dials. It holds no client that dials out to the
/// harness: who may connect is enforced by network policy, so `Run` authenticates
/// structurally. Each `Run` carries one tool call — the assignment first, then
/// the pod streams result frames back and observes an optional cancel — and runs
/// the tool named at construction through the same execution core the pod used
/// as a client.
pub struct ToolJobService {
    tool_name: String,
    scrub: Arc<ScrubSet>,
    activity: Arc<Activity>,
}

impl ToolJobService {
    pub fn new(tool_name: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            scrub: Arc::new(ScrubSet::from_env_var("TOOLSET_SCRUB_SECRETS")),
            activity: Arc::new(Activity::new()),
        }
    }

    /// A handle main waits on to shut the server down: after one call for a
    /// one-shot pod, or after the idle window for a keepalive pod.
    pub fn shutdown_watcher(&self) -> ShutdownWatcher {
        ShutdownWatcher {
            activity: self.activity.clone(),
        }
    }
}

/// Resolves when the server should stop accepting connections.
pub struct ShutdownWatcher {
    activity: Arc<Activity>,
}

impl ShutdownWatcher {
    /// A one-shot pod (keepalive false) exits once its single call finishes; a
    /// keepalive pod exits after `idle_window` elapses with no call in flight,
    /// so a pod the harness stops dialing does not linger to its Job deadline.
    pub async fn wait(self, keepalive: bool, idle_window: Duration) {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let in_flight = self.activity.in_flight.load(Ordering::SeqCst);
            if in_flight > 0 {
                continue;
            }
            if !keepalive {
                if self.activity.completed.load(Ordering::SeqCst) >= 1 {
                    return;
                }
                continue;
            }
            let idle = self.activity.last_activity.lock().await.elapsed();
            if idle >= idle_window {
                return;
            }
        }
    }
}

type FrameStream = Pin<Box<dyn Stream<Item = Result<ToolResultFrame, Status>> + Send>>;

#[tonic::async_trait]
impl ToolJob for ToolJobService {
    type RunStream = FrameStream;

    async fn run(
        &self,
        request: Request<Streaming<RunToolCommand>>,
    ) -> Result<Response<Self::RunStream>, Status> {
        let mut inbound = request.into_inner();

        // The assignment is the first message on the stream. A pod that runs a
        // tool before reading it would run the wrong call.
        let first = inbound
            .next()
            .await
            .ok_or_else(|| {
                Status::invalid_argument("the harness must send an assignment before any command")
            })?
            .map_err(|e| Status::internal(format!("assignment stream error: {e}")))?;
        let assignment = match first.command {
            Some(Command::Assignment(a)) => a,
            _ => {
                return Err(Status::invalid_argument(
                    "the first RunToolCommand must be the tool-call assignment",
                ))
            }
        };

        // A cancel on the request half fires the local token, so the running
        // child is killed on the same held connection — no pod-initiated call.
        let cancel = CancellationToken::new();
        let watch = cancel.clone();
        tokio::spawn(async move {
            while let Some(Ok(cmd)) = inbound.next().await {
                if matches!(cmd.command, Some(Command::Cancel(_))) {
                    watch.cancel();
                    break;
                }
            }
        });

        self.activity.in_flight.fetch_add(1, Ordering::SeqCst);
        self.activity.touch().await;

        let (tx, rx) = tokio::sync::mpsc::channel::<ToolResultFrame>(RESULT_CHANNEL_CAPACITY);
        let tool_name = self.tool_name.clone();
        let scrub = self.scrub.clone();
        let activity = self.activity.clone();
        tokio::spawn(async move {
            let working_dir = if assignment.working_dir.is_empty() {
                "/workspace"
            } else {
                assignment.working_dir.as_str()
            };

            // Builtins complete to one CommandResult framed at once; image tools
            // stream live line-by-line. Both race the cancel token and apply the
            // marker and scrub conventions identically.
            if stdlib::BUILTIN_NAMES.contains(&tool_name.as_str()) {
                let result = stdlib::dispatch_builtin(
                    &tool_name,
                    &assignment.args,
                    working_dir,
                    stdlib::DEFAULT_MAX_OUTPUT_CHARS,
                    &cancel,
                )
                .await;
                for frame in
                    parts::frames_for(&result.stdout, &result.stderr, result.exit_code, &scrub)
                {
                    if tx.send(frame).await.is_err() {
                        break;
                    }
                }
            } else {
                let cmd =
                    execute::compose_dispatch_command(&tool_name, &assignment.args, working_dir);
                execute::stream_frames(cmd, &cancel, None, &scrub, tx).await;
            }

            activity.completed.fetch_add(1, Ordering::SeqCst);
            activity.in_flight.fetch_sub(1, Ordering::SeqCst);
            activity.touch().await;
        });

        let stream = ReceiverStream::new(rx).map(Ok::<ToolResultFrame, Status>);
        Ok(Response::new(Box::pin(stream)))
    }
}
