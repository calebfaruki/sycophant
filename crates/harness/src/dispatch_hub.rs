//! The agent's side of its sub-agents: start each sub-agent's harness as a
//! Job, attach to it, carry its approval requests to the agent's relay and
//! queue its final answer.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use harness_proto::dispatch_link_client::DispatchLinkClient;
use harness_proto::{dispatch_frame, parent_frame, DispatchFrame, ParentFrame, SubAgentMessage};
use k8s_openapi::api::batch::v1::Job;
use proto_common::sub_agent_finished::Outcome;
use proto_common::{
    ClientResponseError, SendServerRequestAndAwaitRequest, SendServerRequestAndAwaitResponse,
    Sender, SubAgentFinished, SubAgentStarted, SubAgentStopped,
};
use relay_proto::deliver_sub_agent_event_request::Event as SubAgentEvent;
use shared::auth::SaTokenInterceptor;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Streaming;

use crate::agent_state::AgentState;
use crate::clients::ServerRequestOutcome;
use crate::conversation::{ConversationLog, EntrySnapshot, LocalFsStore};
use crate::job::build_dispatch_job;
use crate::message_source::InboundMessage;
use crate::tool_router::RelaySeam;

pub(crate) type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Creates one Job in the agent's namespace.
pub(crate) type JobSeam = Arc<dyn Fn(Job) -> BoxFuture<Result<(), String>> + Send + Sync>;

/// Lists the paths of the sub-agents' harnesses that are ready.
pub(crate) type ListSeam = Arc<dyn Fn() -> BoxFuture<Result<Vec<String>, String>> + Send + Sync>;

/// The URI an attach to `<sub>/<id>` dials.
pub(crate) type EndpointSeam = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// How long an attach keeps retrying while the sub-agent's harness starts,
/// its instructions sync included.
pub(crate) const ATTACH_WINDOW: Duration = Duration::from_secs(120);
const ATTACH_BACKOFF: Duration = Duration::from_secs(1);

/// Why a sub-agent finished when its attach ended with no error.
const ATTACH_ENDED: &str = "the sub-agent's harness ended its attach";
/// Why a sub-agent finished when no attach got through.
const NEVER_STARTED: &str = "never started";

/// The JSON-RPC internal error code an approval request gets when the agent's
/// relay cannot be reached.
const INTERNAL_ERROR: i32 = -32603;

/// The requests a sub-agent may send through the agent's relay.
const ALLOWED_METHODS: [&str; 3] = [
    crate::tool_approval::METHOD,
    crate::channel_stream_tools::REQUEST_USER_INPUT,
    crate::channel_stream_tools::REQUEST_USER_AUTH,
];

pub(crate) struct HubConfig {
    pub(crate) agent: String,
    pub(crate) namespace: String,
    /// The harness image every sub-agent's harness runs.
    pub(crate) image: String,
    /// The conversation volume's root. Each sub-agent's folder is
    /// `<root>/<sub>/<id>`.
    pub(crate) root: PathBuf,
    /// The token file each attach presents.
    pub(crate) token_path: PathBuf,
    pub(crate) endpoint: EndpointSeam,
    pub(crate) attach_window: Duration,
}

pub(crate) struct DispatchHub {
    config: HubConfig,
    create_job: JobSeam,
    list: ListSeam,
    relay: RelaySeam,
    agent_state: Arc<AgentState>,
    /// Each open attach's sender, by path, until its final answer.
    attached: std::sync::Mutex<HashMap<String, mpsc::Sender<ParentFrame>>>,
    /// The paths sent a stop.
    stopped: std::sync::Mutex<HashSet<String>>,
}

fn finished(path: String, outcome: Outcome) -> SubAgentEvent {
    SubAgentEvent::Finished(SubAgentFinished {
        path,
        outcome: Some(outcome),
    })
}

/// One open attach: the frames sent to the sub-agent's harness and the ones it
/// sends back.
type Attached = (mpsc::Sender<ParentFrame>, Streaming<DispatchFrame>);

impl DispatchHub {
    pub(crate) fn new(
        config: HubConfig,
        create_job: JobSeam,
        list: ListSeam,
        relay: RelaySeam,
        agent_state: Arc<AgentState>,
    ) -> Self {
        Self {
            config,
            create_job,
            list,
            relay,
            agent_state,
            attached: Default::default(),
            stopped: Default::default(),
        }
    }

    /// Start `sub_agent` with `mandate`: make its Job and its folder, then
    /// attach to it in the background. Returns its path, `<sub>/<id>`, at
    /// once. Admission refuses a Job for a name that is not this agent's
    /// sub-agent, so the folder is made only after the Job is admitted.
    pub(crate) async fn start(
        self: &Arc<Self>,
        sub_agent: &str,
        mandate: &str,
    ) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let path = format!("{sub_agent}/{id}");
        let c = &self.config;
        let job = build_dispatch_job(&c.agent, sub_agent, &id, mandate, &c.namespace, &c.image);
        (self.create_job)(job).await?;
        let folder = c.root.join(&path);
        tokio::fs::create_dir_all(&folder)
            .await
            .map_err(|e| format!("create {}: {e}", folder.display()))?;
        self.send_event(SubAgentEvent::Started(SubAgentStarted {
            path: path.clone(),
        }))
        .await;
        self.attach(path.clone());
        Ok(path)
    }

    /// Attach to the sub-agent's harness at `path` until its final answer is
    /// queued and taken.
    pub(crate) fn attach(self: &Arc<Self>, path: String) -> JoinHandle<()> {
        let hub = self.clone();
        tokio::spawn(async move {
            let deadline = Instant::now() + hub.config.attach_window;
            let attached = loop {
                match hub.try_attach(&path).await {
                    Ok(attached) => break attached,
                    Err(e) if Instant::now() < deadline => {
                        tracing::warn!(path = %path, error = %e, "attach failed, retrying");
                        tokio::time::sleep(ATTACH_BACKOFF).await;
                    }
                    Err(e) => {
                        tracing::error!(path = %path, error = %e, "attach gave up");
                        let failure = Outcome::Failure(NEVER_STARTED.into());
                        hub.send_event(finished(path, failure)).await;
                        return;
                    }
                }
            };
            hub.serve(&path, attached).await;
        })
    }

    /// After the agent's harness restarts: list the ready sub-agents' harnesses,
    /// note them in the conversation and attach to each. Each attach is open
    /// when this returns, unless its first try failed and it retries in the
    /// background.
    pub(crate) async fn restart(self: &Arc<Self>) -> Result<(), String> {
        let paths = (self.list)().await?;
        self.agent_state.note_restart(&paths).await?;
        for path in paths {
            match self.try_attach(&path).await {
                Ok(attached) => {
                    let hub = self.clone();
                    tokio::spawn(async move { hub.serve(&path, attached).await });
                }
                Err(_) => {
                    self.attach(path);
                }
            }
        }
        Ok(())
    }

    async fn try_attach(&self, path: &str) -> Result<Attached, String> {
        let uri = (self.config.endpoint)(path);
        let token = SaTokenInterceptor::new(&self.config.token_path);
        let open = async {
            let channel = tonic::transport::Endpoint::from_shared(uri)
                .map_err(|e| e.to_string())?
                .connect()
                .await
                .map_err(|e| e.to_string())?;
            let (tx, rx) = mpsc::channel(8);
            let frames = DispatchLinkClient::with_interceptor(channel, token)
                .attach(ReceiverStream::new(rx))
                .await
                .map_err(|e| e.to_string())?
                .into_inner();
            Ok((tx, frames))
        };
        tokio::time::timeout(crate::dispatch::DIAL_ATTEMPT_TIMEOUT, open)
            .await
            .map_err(|_| "timed out".to_string())?
    }

    /// Carry the sub-agent's requests to the agent's relay until its final
    /// answer or failure comes. Queue it under `path`, then say it is taken.
    /// A stop asked for before the attach opened is sent once it opens.
    /// An attach that closes first sends stopped if the sub-agent was
    /// stopped, and otherwise finished with why it closed.
    async fn serve(&self, path: &str, (tx, mut frames): Attached) {
        let stop_now = {
            let mut attached = self.attached.lock().unwrap();
            attached.insert(path.to_string(), tx.clone());
            self.stopped.lock().unwrap().contains(path)
        };
        if stop_now {
            let _ = self.send(path, parent_frame::Frame::Stop(true)).await;
        }
        let closed = loop {
            let frame = match frames.message().await {
                Ok(Some(frame)) => frame,
                Ok(None) => break ATTACH_ENDED.to_string(),
                Err(e) => break e.message().to_string(),
            };
            let reply = match frame.frame {
                Some(dispatch_frame::Frame::Request(request)) => {
                    parent_frame::Frame::Response(self.ask(path, request).await)
                }
                Some(dispatch_frame::Frame::FinalAnswer(text)) => {
                    self.finish(path, text.clone(), Outcome::Answer(text)).await;
                    parent_frame::Frame::AnswerTaken(true)
                }
                Some(dispatch_frame::Frame::Failure(reason)) => {
                    let text = format!("dispatch failed: {reason}");
                    self.finish(path, text, Outcome::Failure(reason)).await;
                    parent_frame::Frame::AnswerTaken(true)
                }
                None => continue,
            };
            let taken = matches!(reply, parent_frame::Frame::AnswerTaken(_));
            if tx.send(ParentFrame { frame: Some(reply) }).await.is_err() {
                if taken {
                    return;
                }
                break ATTACH_ENDED.to_string();
            }
            if taken {
                // Hold the attach until the sub-agent's harness exits, so
                // `answer_taken` is not cut off.
                while let Ok(Some(_)) = frames.message().await {}
                return;
            }
        };
        self.attached.lock().unwrap().remove(path);
        tracing::warn!(path = %path, reason = %closed, "attach closed before the final answer");
        let path = path.to_string();
        let event = if self.stopped.lock().unwrap().remove(&path) {
            SubAgentEvent::Stopped(SubAgentStopped { path })
        } else {
            finished(path, Outcome::Failure(closed))
        };
        self.send_event(event).await;
    }

    /// Queue `text` under `path`, from the sub-agent, and send finished with
    /// `outcome`.
    async fn finish(&self, path: &str, text: String, outcome: Outcome) {
        self.attached.lock().unwrap().remove(path);
        self.agent_state
            .queue()
            .push(InboundMessage {
                content: proto_common::text_content(&text),
                reply_channel_stream: Some(path.to_string()),
                sender: Sender::sub_agent(path),
                message_id: path.to_string(),
            })
            .await;
        self.send_event(finished(path.to_string(), outcome)).await;
    }

    async fn send_event(&self, event: SubAgentEvent) {
        if let Err(e) = (self.relay)().deliver_sub_agent_event(event).await {
            tracing::warn!(error = %e, "sub-agent event not sent");
        }
    }

    /// Send one of the sub-agent's requests through the agent's relay, under
    /// the sub-agent's own path, and return the outcome unchanged. Only an
    /// approval and the two requests to the user are sent.
    async fn ask(
        &self,
        path: &str,
        request: SendServerRequestAndAwaitRequest,
    ) -> SendServerRequestAndAwaitResponse {
        let mut response = SendServerRequestAndAwaitResponse::default();
        if !ALLOWED_METHODS.contains(&request.method.as_str()) {
            response.unsupported_method = true;
            return response;
        }
        let mut params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&request.params_json).unwrap_or_default();
        params.insert("dispatch".into(), path.into());
        let params = serde_json::Value::Object(params).to_string();
        let outcome = (self.relay)()
            .send_server_request_and_await(
                &request.request_id,
                &request.method,
                &params,
                request.timeout_seconds,
            )
            .await;
        match outcome {
            Ok(ServerRequestOutcome::Result(json)) => response.result_json = json,
            Ok(ServerRequestOutcome::Error { code, message }) => {
                response.error = Some(ClientResponseError { code, message });
            }
            Ok(ServerRequestOutcome::TimedOut) => response.timed_out = true,
            Ok(ServerRequestOutcome::UnknownChannelStream) => {
                response.unknown_channel_stream = true
            }
            Ok(ServerRequestOutcome::UnsupportedMethod) => response.unsupported_method = true,
            Err(message) => {
                response.error = Some(ClientResponseError {
                    code: INTERNAL_ERROR,
                    message,
                });
            }
        }
        response
    }

    pub(crate) fn agent_sender(&self) -> Sender {
        Sender::parent_agent(self.config.agent.clone())
    }

    /// Send `text` from `sender` to the sub-agent at `path` over its attach.
    pub(crate) async fn message(
        &self,
        path: &str,
        sender: Sender,
        text: &str,
    ) -> Result<(), String> {
        let message = SubAgentMessage {
            text: text.to_string(),
            sender: Some(sender),
        };
        self.send(path, parent_frame::Frame::Message(message)).await
    }

    /// Stop the sub-agent at `path` over its attach.
    pub(crate) async fn stop(&self, path: &str) -> Result<(), String> {
        let gone = || format!("no live sub-agent at {path}");
        let (tx, fresh) = {
            let attached = self.attached.lock().unwrap();
            let tx = attached.get(path).cloned().ok_or_else(gone)?;
            (tx, self.stopped.lock().unwrap().insert(path.to_string()))
        };
        let frame = ParentFrame {
            frame: Some(parent_frame::Frame::Stop(true)),
        };
        if tx.send(frame).await.is_err() {
            if fresh {
                self.stopped.lock().unwrap().remove(path);
            }
            return Err(gone());
        }
        Ok(())
    }

    /// Stop the sub-agent at `path` now if its attach is open, else as soon
    /// as it opens.
    pub(crate) async fn stop_once_attached(&self, path: &str) {
        {
            let attached = self.attached.lock().unwrap();
            if !attached.contains_key(path) {
                self.stopped.lock().unwrap().insert(path.to_string());
                return;
            }
        }
        let _ = self.stop(path).await;
    }

    async fn send(&self, path: &str, frame: parent_frame::Frame) -> Result<(), String> {
        let tx = self.attached.lock().unwrap().get(path).cloned();
        let frame = ParentFrame { frame: Some(frame) };
        match tx {
            Some(tx) if tx.send(frame).await.is_ok() => Ok(()),
            _ => Err(format!("no live sub-agent at {path}")),
        }
    }

    /// The paths of the sub-agents' harnesses that are ready.
    pub(crate) async fn list(&self) -> Result<Vec<String>, String> {
        (self.list)().await
    }

    /// The entries of the `conversation.log` in the sub-agent folder at
    /// `path`, `<sub-agent>/<id>`. Only reads.
    pub(crate) async fn read_log(&self, path: &str) -> Result<Vec<EntrySnapshot>, String> {
        let valid = path.split_once('/').is_some_and(|(sub, id)| {
            let label = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
            let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
            (1..=63).contains(&sub.len())
                && sub.bytes().all(|b| label(b) || b == b'-')
                && sub.bytes().next().is_some_and(label)
                && sub.bytes().last().is_some_and(label)
                && id.len() == 8
                && id.bytes().all(hex)
        });
        if !valid {
            return Err(format!("not a sub-agent path: {path}"));
        }
        let folder = self.config.root.join(path);
        if !tokio::fs::metadata(&folder).await.is_ok_and(|m| m.is_dir()) {
            return Err(format!("no sub-agent at {path}"));
        }
        let store = Arc::new(LocalFsStore::new(folder, 0));
        Ok(ConversationLog::rebuild(store)
            .await?
            .snapshot(None)
            .entries)
    }
}

/// Jobs created through the kube API in `namespace`.
pub(crate) fn kube_jobs(client: kube::Client, namespace: String) -> JobSeam {
    Arc::new(move |job| {
        let (client, namespace) = (client.clone(), namespace.clone());
        Box::pin(async move {
            crate::job::create_job(&client, &namespace, &job)
                .await
                .map(drop)
                .map_err(|e| format!("create the Job: {e}"))
        })
    })
}

/// `<sub>-<id>.ag-<agent>-dispatch.<ns>.svc.cluster.local:9092` for path
/// `<sub>/<id>`.
pub(crate) fn cluster_endpoint(agent: String, namespace: String) -> EndpointSeam {
    Arc::new(move |path| {
        let host = path.replacen('/', "-", 1);
        let port = crate::DISPATCH_LINK_PORT;
        format!("http://{host}.ag-{agent}-dispatch.{namespace}.svc.cluster.local:{port}")
    })
}

/// One SRV query on the agent's headless Service. It lists ready pods only, and
/// each pod's hostname `<sub>-<id>` reads back as path `<sub>/<id>`.
pub(crate) fn srv_list(agent: String, namespace: String) -> ListSeam {
    Arc::new(move || {
        let name = format!("_dispatch._tcp.ag-{agent}-dispatch.{namespace}.svc.cluster.local.");
        Box::pin(async move {
            let resolver = hickory_resolver::TokioResolver::builder_tokio()
                .map_err(|e| format!("DNS config: {e}"))?
                .build();
            let records = match resolver.srv_lookup(name.as_str()).await {
                Ok(records) => records,
                Err(e) if e.is_nx_domain() || e.is_no_records_found() => return Ok(vec![]),
                Err(e) => return Err(format!("SRV {name}: {e}")),
            };
            Ok(records
                .iter()
                .filter_map(|srv| {
                    let host = srv.target().iter().next()?;
                    let (sub, id) = std::str::from_utf8(host).ok()?.rsplit_once('-')?;
                    Some(format!("{sub}/{id}"))
                })
                .collect())
        })
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use shared::auth::TokenVerifier;
    use tokio_stream::StreamExt;
    use tonic::transport::server::TcpIncoming;
    use tonic::transport::Server;
    use tonic::Status;

    use super::*;
    use crate::dispatch_link::ParentLink;
    use crate::queue::Inbox;
    use crate::test_doubles::{empty_agent_state, ScriptedRelay};
    use crate::tool_router::relay_seam;

    pub(crate) const AGENT: &str = "one";
    const NAMESPACE: &str = "tenant-a";
    /// Agent `one`'s harness, `ag-one` in namespace `tenant-a`.
    pub(crate) const PARENT_TOKEN: &str = "tok-system:serviceaccount:tenant-a:ag-one";

    /// TokenReview stand-in: token `tok-<username>` authenticates as
    /// `<username>`.
    struct UserTokens;

    #[async_trait::async_trait]
    impl TokenVerifier for UserTokens {
        async fn verify_token(&self, token: &str) -> Result<String, Status> {
            token
                .strip_prefix("tok-")
                .map(str::to_string)
                .ok_or_else(|| Status::permission_denied("unknown token"))
        }
    }

    /// The sub-agent's side, admitting only agent `one`'s harness.
    pub(crate) fn sub_agent_link() -> ParentLink {
        sub_agent_link_with(Inbox::default())
    }

    /// [`sub_agent_link`], its messages landing in `inbox`.
    pub(crate) fn sub_agent_link_with(inbox: Inbox) -> ParentLink {
        ParentLink::new(AGENT, NAMESPACE, Arc::new(UserTokens), inbox)
    }

    /// What lands in `inbox` within 5s, each with its sender: empty if
    /// nothing does.
    pub(crate) async fn landed_from(inbox: &Inbox) -> Vec<(Sender, String)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let items = inbox.drain();
            if !items.is_empty() || Instant::now() >= deadline {
                return items;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    pub(crate) async fn landed(inbox: &Inbox) -> Vec<String> {
        landed_from(inbox)
            .await
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    }

    /// Wait until the hub's attach to `path` is open.
    pub(crate) async fn attached(h: &TestHub, path: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !h.hub.attached.lock().unwrap().contains_key(path) {
            assert!(Instant::now() < deadline, "the hub never attached {path}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Serve `link` on an ephemeral port and return its URI.
    pub(crate) fn serve(link: ParentLink) -> String {
        let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = incoming.local_addr().unwrap();
        tokio::spawn(
            Server::builder()
                .add_service(harness_proto::dispatch_link_server::DispatchLinkServer::new(link))
                .serve_with_incoming(incoming),
        );
        format!("http://{addr}")
    }

    /// A URI that accepts the connection and never answers, so no attach
    /// gets through.
    pub(crate) fn silent_endpoint() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::mem::forget(listener);
        format!("http://{addr}")
    }

    /// An attach to a path not in `uris` dials `fallback`.
    pub(crate) fn endpoints(uris: Vec<(&str, String)>, fallback: String) -> EndpointSeam {
        let uris: HashMap<String, String> =
            uris.into_iter().map(|(p, u)| (p.to_string(), u)).collect();
        Arc::new(move |path| uris.get(path).cloned().unwrap_or(fallback.clone()))
    }

    /// One attach: what the parent sends, and what the sub-agent sends back.
    pub(crate) struct ParentAttach {
        pub(crate) tx: mpsc::Sender<ParentFrame>,
        pub(crate) frames: Streaming<DispatchFrame>,
    }

    impl ParentAttach {
        pub(crate) async fn next_frame(&mut self) -> dispatch_frame::Frame {
            tokio::time::timeout(Duration::from_secs(5), self.frames.next())
                .await
                .expect("a frame within 5s")
                .expect("the attach is open")
                .expect("a frame, not an error")
                .frame
                .expect("a frame body")
        }
    }

    /// Attach to `uri` with `Bearer <token>`, or with no token.
    pub(crate) async fn attach_as(uri: &str, token: Option<&str>) -> Result<ParentAttach, Status> {
        let mut client = DispatchLinkClient::connect(uri.to_string())
            .await
            .expect("the sub-agent's harness serves");
        let (tx, rx) = mpsc::channel(8);
        let mut request = tonic::Request::new(ReceiverStream::new(rx));
        if let Some(token) = token {
            request
                .metadata_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
        }
        let frames = client.attach(request).await?.into_inner();
        Ok(ParentAttach { tx, frames })
    }

    pub(crate) struct TestHub {
        pub(crate) hub: Arc<DispatchHub>,
        pub(crate) jobs: Arc<Mutex<Vec<Job>>>,
        pub(crate) root: PathBuf,
        pub(crate) agent_state: Arc<AgentState>,
    }

    /// Agent `one`'s hub. Admission admits only sub-agent `sub`'s Jobs,
    /// which are recorded. `listed` is what a restart finds ready. An attach
    /// stops retrying after 100ms. The volume root sits in a folder of its
    /// own, so a path that climbs out of it reaches only this test's files.
    pub(crate) fn test_hub(
        endpoint: EndpointSeam,
        relay: ScriptedRelay,
        listed: Vec<String>,
    ) -> TestHub {
        let root = tempfile::TempDir::new().unwrap().keep().join("vol");
        std::fs::create_dir(&root).unwrap();
        let token_path = root.join("token");
        std::fs::write(&token_path, PARENT_TOKEN).unwrap();
        let jobs = Arc::new(Mutex::new(Vec::new()));
        let record = jobs.clone();
        let create_job: JobSeam = Arc::new(move |job| {
            let admitted = job
                .metadata
                .name
                .as_deref()
                .is_some_and(|n| n.starts_with("sub-"));
            if !admitted {
                return Box::pin(async { Err("admission refused the Job".to_string()) });
            }
            record.lock().unwrap().push(job);
            Box::pin(async { Ok(()) })
        });
        let list: ListSeam = Arc::new(move || {
            let listed = listed.clone();
            Box::pin(async move { Ok(listed) })
        });
        let agent_state = Arc::new(empty_agent_state());
        let config = HubConfig {
            agent: AGENT.into(),
            namespace: NAMESPACE.into(),
            image: "sycophant-harness:local".into(),
            root: root.clone(),
            token_path,
            endpoint,
            attach_window: Duration::from_millis(100),
        };
        let hub = DispatchHub::new(
            config,
            create_job,
            list,
            relay_seam(relay),
            agent_state.clone(),
        );
        TestHub {
            hub: Arc::new(hub),
            jobs,
            root,
            agent_state,
        }
    }

    /// The queued item, waiting up to 5s for one.
    pub(crate) async fn next_queued(agent_state: &AgentState) -> InboundMessageView {
        let queue = agent_state.queue();
        tokio::time::timeout(std::time::Duration::from_secs(5), queue.wait_ready())
            .await
            .expect("an item is queued within 5s");
        let m = queue.pop().await.expect("a queued item");
        InboundMessageView {
            sender: m.sender,
            reply_channel_stream: m.reply_channel_stream,
            text: proto_common::content_text(&m.content),
        }
    }

    #[derive(Debug, PartialEq)]
    pub(crate) struct InboundMessageView {
        pub(crate) sender: proto_common::Sender,
        pub(crate) reply_channel_stream: Option<String>,
        pub(crate) text: String,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use harness_proto::dispatch_link_server::{DispatchLink, DispatchLinkServer};
    use harness_proto::{DispatchFrame, ParentFrame};
    use proto_common::sub_agent_finished::Outcome;
    use proto_common::{
        text_content, EntryKind, Sender, SubAgentFinished, SubAgentStarted, SubAgentStopped,
        TurnState, TurnStateEvent,
    };
    use relay_proto::deliver_sub_agent_event_request::Event;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Instant};
    use tokio_stream::wrappers::ReceiverStream;
    use tonic::transport::server::TcpIncoming;
    use tonic::transport::Server;
    use tonic::{Status, Streaming};

    use super::test_support::*;
    use crate::clients::{RelayRpc, ServerRequestOutcome};
    use crate::message_source::InboundMessage;
    use crate::queue::Inbox;
    use crate::test_doubles::{RelayAnswer, ScriptedRelay};

    const PATH: &str = "sub/0a1b2c3d";
    const OTHER: &str = "two/12345678";
    const APPROVAL: &str = "RequestToolApproval";

    fn view(sender: Sender, reply: &str, text: &str) -> InboundMessageView {
        InboundMessageView {
            sender,
            reply_channel_stream: Some(reply.into()),
            text: text.into(),
        }
    }

    fn started(path: &str) -> Event {
        Event::Started(SubAgentStarted { path: path.into() })
    }

    fn stopped(path: &str) -> Event {
        Event::Stopped(SubAgentStopped { path: path.into() })
    }

    /// The relay's sub-agent events 200ms after the `n`th, or after 5s.
    async fn events(relay: &ScriptedRelay, n: usize) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while relay.sub_agent_events().len() < n && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        relay.sub_agent_events()
    }

    /// A sub-agent's harness the test drives. Its one attach gets what is
    /// sent on `frames` and ends when `frames` drops.
    struct Driven {
        uri: String,
        frames: mpsc::Sender<Result<DispatchFrame, Status>>,
    }

    struct DrivenLink {
        frames: Mutex<Option<mpsc::Receiver<Result<DispatchFrame, Status>>>>,
    }

    #[tonic::async_trait]
    impl DispatchLink for DrivenLink {
        type AttachStream = ReceiverStream<Result<DispatchFrame, Status>>;

        async fn attach(
            &self,
            request: tonic::Request<Streaming<ParentFrame>>,
        ) -> Result<tonic::Response<Self::AttachStream>, Status> {
            let frames = self.frames.lock().unwrap().take();
            let frames = frames.ok_or_else(|| Status::unavailable("attached already"))?;
            let mut inbound = request.into_inner();
            tokio::spawn(async move { while let Ok(Some(_)) = inbound.message().await {} });
            Ok(tonic::Response::new(ReceiverStream::new(frames)))
        }
    }

    fn driven() -> Driven {
        let (frames, rx) = mpsc::channel(8);
        let link = DrivenLink {
            frames: Mutex::new(Some(rx)),
        };
        let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let uri = format!("http://{}", incoming.local_addr().unwrap());
        tokio::spawn(
            Server::builder()
                .add_service(DispatchLinkServer::new(link))
                .serve_with_incoming(incoming),
        );
        Driven { uri, frames }
    }

    #[tokio::test]
    async fn a_final_answer_queues_behind_earlier_messages_with_its_path_then_is_taken() {
        let link = sub_agent_link();
        let mut held = link.clone();
        held.deliver_reply(EntryKind::Message, text_content("the answer"))
            .await
            .unwrap();
        let uris = endpoints(vec![(PATH, serve(link.clone()))], silent_endpoint());
        let h = test_hub(uris, ScriptedRelay::default(), vec![]);
        h.agent_state
            .queue()
            .push(InboundMessage {
                content: text_content("hi"),
                reply_channel_stream: Some("person".into()),
                sender: Sender::person("person"),
                message_id: "m1".into(),
            })
            .await;

        h.hub.attach(PATH.into());
        timeout(Duration::from_secs(5), link.answer_taken())
            .await
            .expect("the hub takes the answer");

        assert_eq!(
            next_queued(&h.agent_state).await,
            view(Sender::person("person"), "person", "hi"),
            "the person's message, queued first, goes first"
        );
        assert_eq!(
            next_queued(&h.agent_state).await,
            view(Sender::sub_agent(PATH), PATH, "the answer"),
            "the answer is queued under its path, from the sub-agent, before it is taken"
        );
        let entries = h
            .agent_state
            .log()
            .read()
            .await
            .history_for_provider()
            .len();
        assert_eq!(entries, 0, "the answer writes no log entry");
    }

    #[tokio::test]
    async fn a_sub_agent_request_reaches_the_agent_relay_under_its_own_path_and_its_outcome_returns(
    ) {
        let link = sub_agent_link();
        let uris = endpoints(vec![(PATH, serve(link.clone()))], silent_endpoint());
        let relay = ScriptedRelay::new(vec![RelayAnswer::approve()]);
        let h = test_hub(uris, relay.clone(), vec![]);
        h.hub.attach(PATH.into());

        let mut ask = link.clone();
        let params = r#"{"tool":"Bash","input":"{}","dispatch":"evil/ffffffff"}"#;
        let call = ask.send_server_request_and_await("c1", APPROVAL, params, 0);
        let outcome = timeout(Duration::from_secs(5), call)
            .await
            .expect("the request is answered within 5s");

        assert!(
            matches!(&outcome, Ok(ServerRequestOutcome::Result(r)) if r == r#"{"approved":true}"#),
            "the relay's outcome comes back unchanged, got {outcome:?}"
        );
        let sent: Vec<serde_json::Value> = relay
            .requests_for(APPROVAL)
            .iter()
            .map(|r| serde_json::from_str(&r.params_json).unwrap())
            .collect();
        let got: Vec<(&str, &str)> = sent
            .iter()
            .map(|p| (p["dispatch"].as_str().unwrap(), p["tool"].as_str().unwrap()))
            .collect();
        assert_eq!(got, [(PATH, "Bash")], "the path overwrites the one sent");
    }

    /// An older attach may still be open when the restarted harness attaches.
    #[tokio::test]
    async fn a_restart_attaches_each_listed_path_ahead_of_an_older_attach() {
        let held = sub_agent_link();
        let asking = sub_agent_link();
        let uris = endpoints(
            vec![(PATH, serve(held.clone())), (OTHER, serve(asking.clone()))],
            silent_endpoint(),
        );
        let mut reply = held.clone();
        reply
            .deliver_reply(EntryKind::Message, text_content("the answer"))
            .await
            .unwrap();
        let _older = (
            attach_as(&uris(PATH), Some(PARENT_TOKEN))
                .await
                .expect("admitted"),
            attach_as(&uris(OTHER), Some(PARENT_TOKEN))
                .await
                .expect("admitted"),
        );
        let relay = ScriptedRelay::new(vec![RelayAnswer::approve()]);
        let h = test_hub(uris, relay.clone(), vec![PATH.into(), OTHER.into()]);

        h.hub.restart().await.expect("the restart attaches");

        assert_eq!(
            next_queued(&h.agent_state).await,
            view(Sender::sub_agent(PATH), PATH, "the answer"),
            "the held answer reaches the new attach"
        );
        let mut ask = asking.clone();
        let call = ask.send_server_request_and_await("c1", APPROVAL, "{}", 0);
        let outcome = timeout(Duration::from_secs(5), call).await;
        assert!(
            matches!(&outcome, Ok(Ok(ServerRequestOutcome::Result(_)))),
            "a new request goes to the new attach, got {outcome:?}"
        );
        assert_eq!(relay.requests_for(APPROVAL).len(), 1);
    }

    #[tokio::test]
    async fn cancelling_the_agents_turn_leaves_its_sub_agents_attached() {
        let inbox = Inbox::default();
        let link = sub_agent_link_with(inbox.clone());
        let uris = endpoints(vec![(PATH, serve(link.clone()))], silent_endpoint());
        let h = test_hub(uris, ScriptedRelay::default(), vec![]);
        h.hub.attach(PATH.into());
        attached(&h, PATH).await;
        h.hub
            .message(PATH, h.hub.agent_sender(), "before")
            .await
            .unwrap();
        assert_eq!(landed(&inbox).await, ["before"]);

        let _turn = h.agent_state.register_turn();
        assert!(h.agent_state.cancel(), "the agent's turn is cancelled");

        let after = h.hub.message(PATH, h.hub.agent_sender(), "after").await;
        assert_eq!(after, Ok(()), "the attach is still open");
        assert_eq!(
            landed(&inbox).await,
            ["after"],
            "the cancel sent the sub-agent no message"
        );
        let stopped = timeout(Duration::from_millis(200), link.stopped()).await;
        assert!(stopped.is_err(), "the cancel did not stop the sub-agent");
    }

    #[tokio::test]
    async fn the_hub_forwards_only_allow_listed_requests() {
        let link = sub_agent_link();
        let uris = endpoints(vec![(PATH, serve(link.clone()))], silent_endpoint());
        let relay = ScriptedRelay::new(vec![RelayAnswer::approve(), RelayAnswer::approve()]);
        let h = test_hub(uris, relay.clone(), vec![]);
        let turn = h.agent_state.register_turn();
        h.hub.attach(PATH.into());

        let mut ask = link.clone();
        let mut outcomes = Vec::new();
        for method in ["CancelTurn", "RequestUserInput"] {
            let call = ask.send_server_request_and_await("c1", method, "{}", 0);
            let outcome = timeout(Duration::from_secs(5), call).await;
            outcomes.push(outcome.expect("the request is answered within 5s"));
        }

        assert!(
            matches!(outcomes[0], Ok(ServerRequestOutcome::UnsupportedMethod)),
            "CancelTurn gets unsupported_method, got {:?}",
            outcomes[0]
        );
        assert!(
            relay.requests_for("CancelTurn").is_empty(),
            "CancelTurn never reaches the relay"
        );
        assert!(!turn.is_cancelled(), "the agent's turn runs on");
        assert_eq!(
            relay.requests_for("RequestUserInput").len(),
            1,
            "RequestUserInput still reaches the relay"
        );
    }

    #[tokio::test]
    async fn start_sends_one_started_event_naming_the_path_and_a_restart_sends_none() {
        let relay = ScriptedRelay::default();
        let uris = endpoints(vec![], silent_endpoint());
        let h = test_hub(uris, relay.clone(), vec![OTHER.into()]);

        let path = h
            .hub
            .start("sub", "summarize the repo")
            .await
            .expect("the sub-agent starts");
        h.hub.restart().await.expect("the restart attaches");

        let starts: Vec<Event> = events(&relay, 1)
            .await
            .into_iter()
            .filter(|e| matches!(e, Event::Started(_)))
            .collect();
        assert_eq!(starts, [started(&path)], "only the start sends started");
    }

    #[tokio::test]
    async fn a_final_answer_sends_finished_with_the_answer() {
        let link = sub_agent_link();
        let mut held = link.clone();
        held.deliver_reply(EntryKind::Message, text_content("the answer"))
            .await
            .unwrap();
        let relay = ScriptedRelay::default();
        let uris = endpoints(vec![(PATH, serve(link))], silent_endpoint());
        let h = test_hub(uris, relay.clone(), vec![]);

        h.hub.attach(PATH.into());

        assert_eq!(
            events(&relay, 1).await,
            [super::finished(
                PATH.into(),
                Outcome::Answer("the answer".into())
            )]
        );
    }

    #[tokio::test]
    async fn a_failed_turn_sends_finished_with_its_reason_and_still_queues_the_failure_as_text() {
        let link = sub_agent_link();
        let mut held = link.clone();
        held.deliver_turn_state(TurnStateEvent {
            state: TurnState::Failed as i32,
            reason: "model call failed".into(),
            code: "13".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        let relay = ScriptedRelay::default();
        let uris = endpoints(vec![(PATH, serve(link))], silent_endpoint());
        let h = test_hub(uris, relay.clone(), vec![]);

        h.hub.attach(PATH.into());

        assert_eq!(
            events(&relay, 1).await,
            [super::finished(
                PATH.into(),
                Outcome::Failure("model call failed".into())
            )]
        );
        assert_eq!(
            next_queued(&h.agent_state).await.text,
            "dispatch failed: model call failed",
            "the agent still reads the failure as text"
        );
    }

    #[tokio::test]
    async fn an_attach_that_ends_before_an_answer_sends_finished_with_a_failure_and_why() {
        let died = driven();
        let ended = driven();
        let uris = endpoints(
            vec![(PATH, died.uri.clone()), (OTHER, ended.uri.clone())],
            silent_endpoint(),
        );
        let relay = ScriptedRelay::default();
        let h = test_hub(uris, relay.clone(), vec![]);
        for path in [PATH, OTHER] {
            h.hub.attach(path.into());
            attached(&h, path).await;
        }

        died.frames
            .send(Err(Status::unavailable("the pod died")))
            .await
            .unwrap();
        drop(ended.frames);

        let got = events(&relay, 2).await;
        assert_eq!(got.len(), 2, "one event per path, got {got:?}");
        let why = |path: &str| {
            got.iter()
                .find_map(|e| match e {
                    Event::Finished(SubAgentFinished {
                        path: p,
                        outcome: Some(Outcome::Failure(why)),
                    }) if p == path => Some(why.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no finished failure for {path} in {got:?}"))
        };
        assert!(
            why(PATH).contains("the pod died"),
            "the error is the reason, got {:?}",
            why(PATH)
        );
        assert!(!why(OTHER).is_empty(), "a clean end still says why");
    }

    #[tokio::test]
    async fn an_attach_that_gives_up_sends_finished_with_the_failure_never_started() {
        let relay = ScriptedRelay::default();
        let h = test_hub(endpoints(vec![], silent_endpoint()), relay.clone(), vec![]);

        h.hub.attach(PATH.into());

        assert_eq!(
            events(&relay, 1).await,
            [super::finished(
                PATH.into(),
                Outcome::Failure("never started".into())
            )]
        );
    }

    #[tokio::test]
    async fn a_start_that_cannot_make_its_folder_sends_no_started_event() {
        let relay = ScriptedRelay::default();
        let h = test_hub(endpoints(vec![], silent_endpoint()), relay.clone(), vec![]);
        std::fs::write(h.root.join("sub"), "").unwrap();

        let err = h.hub.start("sub", "summarize the repo").await.unwrap_err();
        assert!(err.starts_with("create "), "{err}");
        assert_eq!(relay.sub_agent_events(), Vec::<Event>::new());
    }

    #[tokio::test]
    async fn a_stopped_sub_agent_sends_only_a_stopped_event_when_its_attach_ends() {
        let link = driven();
        let relay = ScriptedRelay::default();
        let uris = endpoints(vec![(PATH, link.uri.clone())], silent_endpoint());
        let h = test_hub(uris, relay.clone(), vec![]);
        h.hub.attach(PATH.into());
        attached(&h, PATH).await;

        h.hub.stop(PATH).await.expect("the attach is open");
        drop(link.frames);

        assert_eq!(events(&relay, 1).await, [stopped(PATH)]);
    }

    #[tokio::test]
    async fn a_failed_stop_leaves_a_later_close_reported_as_finished() {
        let link = driven();
        let relay = ScriptedRelay::default();
        let uris = endpoints(vec![(PATH, link.uri.clone())], silent_endpoint());
        let h = test_hub(uris, relay.clone(), vec![]);

        assert!(
            h.hub.stop(PATH).await.is_err(),
            "a stop before the attach opens fails"
        );
        h.hub.attach(PATH.into());
        attached(&h, PATH).await;
        drop(link.frames);

        let got = events(&relay, 1).await;
        assert!(
            matches!(
                got.as_slice(),
                [Event::Finished(SubAgentFinished {
                    outcome: Some(Outcome::Failure(_)),
                    ..
                })]
            ),
            "the close is reported as finished with a failure, not stopped, got {got:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_stop_keeps_a_stop_asked_for_before_the_attach_opened() {
        let link = sub_agent_link();
        let uris = endpoints(vec![(PATH, serve(link.clone()))], silent_endpoint());
        let h = test_hub(uris, ScriptedRelay::default(), vec![]);

        h.hub.stop_once_attached(PATH).await;
        assert!(
            h.hub.stop(PATH).await.is_err(),
            "a stop before the attach opens fails"
        );
        h.hub.attach(PATH.into());

        timeout(Duration::from_secs(5), link.stopped())
            .await
            .expect("the sub-agent is stopped once its attach opens");
    }
}
