//! Dispatch mode's link to the parent agent's harness.
//!
//! A dispatch serves `DispatchLink` and the parent's harness attaches to it.
//! The link carries the dispatch's approval requests up to the parent and
//! holds the dispatch's final answer until the parent takes it.

use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

use harness_proto::dispatch_link_server::DispatchLink;
use harness_proto::{dispatch_frame, parent_frame, DispatchFrame, ParentFrame};
use proto_common::{
    ContentBlock, SendServerRequestAndAwaitRequest, SendServerRequestAndAwaitResponse, StreamItem,
    TurnState, TurnStateEvent,
};
use shared::auth::{
    extract_bearer_token, K8sTokenVerifier, TokenVerifier, HARNESS_DISPATCH_AUDIENCE,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_stream::wrappers::{ReceiverStream, WatchStream};
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

use crate::clients::{RelayRpc, ServerRequestOutcome};
use crate::message_source::{InboundMessage, MessageSource};

const ATTACH_DROPPED: &str = "the parent's attach dropped";

/// One attached parent: where frames go, and the request waiting on it.
/// The dispatch's one turn loop sends one request at a time, so the next
/// response answers it.
struct Attach {
    frames: mpsc::Sender<Result<DispatchFrame, Status>>,
    waiting: Mutex<Option<oneshot::Sender<SendServerRequestAndAwaitResponse>>>,
}

struct Link {
    parent: String,
    namespace: String,
    verifier: Arc<dyn TokenVerifier>,
    answer: watch::Sender<Option<String>>,
    /// The newest attach. Only its inbound task holds it, so it is gone
    /// once that stream ends.
    attached: watch::Sender<Option<Weak<Attach>>>,
    taken: watch::Sender<bool>,
}

/// The dispatch's `RelayRpc`: a link to the attached parent agent's harness.
#[derive(Clone)]
pub(crate) struct ParentLink {
    link: Arc<Link>,
}

impl ParentLink {
    /// `parent` is the only agent whose harness may attach, and only from
    /// `namespace`. `verifier` returns the caller's TokenReview username.
    pub(crate) fn new(
        parent: impl Into<String>,
        namespace: impl Into<String>,
        verifier: Arc<dyn TokenVerifier>,
    ) -> Self {
        Self {
            link: Arc::new(Link {
                parent: parent.into(),
                namespace: namespace.into(),
                verifier,
                answer: watch::channel(None).0,
                attached: watch::channel(None).0,
                taken: watch::channel(false).0,
            }),
        }
    }

    /// Resolves once the parent sends `answer_taken`.
    pub(crate) async fn answer_taken(&self) {
        let _ = self.link.taken.subscribe().wait_for(|taken| *taken).await;
    }
}

#[async_trait::async_trait]
impl RelayRpc for ParentLink {
    async fn send_server_notification(
        &mut self,
        _method: &str,
        _params_json: &str,
    ) -> Result<bool, String> {
        Ok(false)
    }

    async fn send_server_request_and_await(
        &mut self,
        request_id: &str,
        method: &str,
        params_json: &str,
        timeout_seconds: u32,
    ) -> Result<ServerRequestOutcome, String> {
        let attach = self
            .link
            .attached
            .subscribe()
            .wait_for(|a| a.as_ref().is_some_and(|a| a.strong_count() > 0))
            .await
            .map_err(|e| e.to_string())?
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or(ATTACH_DROPPED)?;
        let (tx, done) = oneshot::channel();
        *attach.waiting.lock().unwrap() = Some(tx);
        let request = SendServerRequestAndAwaitRequest {
            request_id: request_id.to_string(),
            method: method.to_string(),
            params_json: params_json.to_string(),
            timeout_seconds,
        };
        attach
            .frames
            .send(Ok(DispatchFrame {
                frame: Some(dispatch_frame::Frame::Request(request)),
            }))
            .await
            .map_err(|_| ATTACH_DROPPED)?;
        // Holding the attach would keep its answer slot open after it drops.
        drop(attach);
        let response = done.await.map_err(|_| ATTACH_DROPPED)?;
        Ok(response.into())
    }

    /// A dispatch streams nothing to its parent.
    async fn deliver_stream_item(&mut self, _item: StreamItem) -> Result<bool, String> {
        Ok(false)
    }

    async fn deliver_reply(&mut self, content: Vec<ContentBlock>) -> Result<bool, String> {
        let text = proto_common::content_text(&content);
        self.link.answer.send_replace(Some(text));
        Ok(true)
    }

    /// A turn that ends with no reply holds `dispatch failed: <reason>`.
    async fn deliver_turn_state(&mut self, turn_state: TurnStateEvent) -> Result<bool, String> {
        let ended = turn_state.state != TurnState::Working as i32;
        if ended && self.link.answer.borrow().is_none() {
            let failed = format!("dispatch failed: {}", turn_state.reason);
            self.link.answer.send_replace(Some(failed));
        }
        Ok(true)
    }
}

type FrameStream = Pin<Box<dyn Stream<Item = Result<DispatchFrame, Status>> + Send>>;

#[tonic::async_trait]
impl DispatchLink for ParentLink {
    type AttachStream = FrameStream;

    async fn attach(
        &self,
        request: Request<Streaming<ParentFrame>>,
    ) -> Result<Response<Self::AttachStream>, Status> {
        let token = extract_bearer_token(&request)?.to_string();
        let username = self.link.verifier.verify_token(&token).await?;
        let parent = format!(
            "system:serviceaccount:{}:ag-{}",
            self.link.namespace, self.link.parent
        );
        if username != parent {
            return Err(Status::permission_denied(format!(
                "{username} is not this dispatch's parent"
            )));
        }

        let mut inbound = request.into_inner();
        let (frames, rx) = mpsc::channel(16);
        let attach = Arc::new(Attach {
            frames,
            waiting: Mutex::new(None),
        });
        self.link
            .attached
            .send_replace(Some(Arc::downgrade(&attach)));

        let link = self.link.clone();
        tokio::spawn(async move {
            while let Some(Ok(frame)) = inbound.next().await {
                match frame.frame {
                    Some(parent_frame::Frame::Response(response)) => {
                        let waiter = attach.waiting.lock().unwrap().take();
                        if let Some(waiter) = waiter {
                            let _ = waiter.send(response);
                        }
                    }
                    Some(parent_frame::Frame::AnswerTaken(true)) => {
                        link.taken.send_replace(true);
                    }
                    _ => {}
                }
            }
            // Dropping the attach here ends a request still waiting on it.
        });

        // The final answer goes to every attach, once it is held.
        let answer = WatchStream::new(self.link.answer.subscribe())
            .filter_map(|answer| answer)
            .map(|text| DispatchFrame {
                frame: Some(dispatch_frame::Frame::FinalAnswer(text)),
            })
            .map(Ok);
        let frames = ReceiverStream::new(rx).merge(answer);
        Ok(Response::new(Box::pin(frames)))
    }
}

/// The verifier `Attach` checks the parent's token with.
pub(crate) fn dispatch_verifier(client: kube::Client) -> K8sTokenVerifier {
    K8sTokenVerifier::new(client, HARNESS_DISPATCH_AUDIENCE)
}

/// The dispatch's message source: its mandate, once.
pub(crate) struct MandateSource {
    mandate: Option<InboundMessage>,
}

impl MandateSource {
    /// `path` is the dispatch's `<sub-agent>/<id>`.
    pub(crate) fn new(path: impl Into<String>, mandate: impl Into<String>) -> Self {
        let path = path.into();
        Self {
            mandate: Some(InboundMessage {
                content: proto_common::text_content(&mandate.into()),
                reply_channel_stream: Some(path.clone()),
                sender: path.clone(),
                message_id: path,
            }),
        }
    }
}

#[async_trait::async_trait]
impl MessageSource for MandateSource {
    async fn next_message(&mut self) -> Result<InboundMessage, String> {
        match self.mandate.take() {
            Some(mandate) => Ok(mandate),
            None => std::future::pending().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use harness_proto::dispatch_frame::Frame;
    use harness_proto::dispatch_link_client::DispatchLinkClient;
    use harness_proto::dispatch_link_server::DispatchLinkServer;
    use harness_proto::parent_frame;
    use proto_common::{StopReason, TurnState};
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_stream::StreamExt;
    use tonic::transport::server::TcpIncoming;
    use tonic::transport::Server;
    use tonic::Code;

    use super::*;
    use crate::runtime_entrypoint::message_loop;
    use crate::test_doubles::{
        complete_turn, empty_agent_state, test_instructions, test_router, ScriptedTurns, TEST_AG,
    };

    const PARENT: &str = "one";
    const NAMESPACE: &str = "tenant-a";
    const PATH: &str = "sub/0a1b2c3d";
    /// The parent agent's harness, `ag-one` in this dispatch's namespace.
    const PARENT_TOKEN: &str = "tok-system:serviceaccount:tenant-a:ag-one";

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

    #[tokio::test]
    async fn dispatch_verifier_pins_the_dispatch_audience() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let config = kube::Config::new("http://127.0.0.1:1".parse().unwrap());
        let client = kube::Client::try_from(config).unwrap();
        assert_eq!(
            dispatch_verifier(client).audience(),
            HARNESS_DISPATCH_AUDIENCE
        );
    }

    fn link() -> ParentLink {
        ParentLink::new(PARENT, NAMESPACE, Arc::new(UserTokens))
    }

    /// Serve `link` on an ephemeral port and return its dial endpoint.
    fn serve(link: ParentLink) -> String {
        let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = incoming.local_addr().unwrap();
        tokio::spawn(
            Server::builder()
                .add_service(DispatchLinkServer::new(link))
                .serve_with_incoming(incoming),
        );
        format!("http://{addr}")
    }

    /// One attach: what the parent sends, and what the dispatch sends back.
    struct Attached {
        tx: mpsc::Sender<ParentFrame>,
        frames: Streaming<DispatchFrame>,
    }

    impl Attached {
        async fn next_frame(&mut self) -> Frame {
            tokio::time::timeout(Duration::from_secs(5), self.frames.next())
                .await
                .expect("a frame within 5s")
                .expect("the attach is open")
                .expect("a frame, not an error")
                .frame
                .expect("a frame body")
        }
    }

    /// Attach to `endpoint` with `Bearer <token>`, or with no token.
    async fn attach_as(endpoint: &str, token: Option<&str>) -> Result<Attached, Status> {
        let mut client = DispatchLinkClient::connect(endpoint.to_string())
            .await
            .expect("the dispatch link serves");
        let (tx, rx) = mpsc::channel(8);
        let mut request = Request::new(ReceiverStream::new(rx));
        if let Some(token) = token {
            request
                .metadata_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
        }
        let frames = client.attach(request).await?.into_inner();
        Ok(Attached { tx, frames })
    }

    fn final_answer(text: &str) -> Frame {
        Frame::FinalAnswer(text.to_string())
    }

    #[tokio::test]
    async fn attach_admits_only_the_parent_harness_service_account_in_its_own_namespace() {
        let endpoint = serve(link());
        if let Err(e) = attach_as(&endpoint, Some(PARENT_TOKEN)).await {
            panic!("the parent agent's harness must be admitted, got {e:?}");
        }
        let mut admitted = Vec::new();
        for (who, token) in [
            (
                "a sibling sub-agent",
                Some("tok-system:serviceaccount:tenant-a:ag-two"),
            ),
            (
                "another agent",
                Some("tok-system:serviceaccount:tenant-a:ag-other"),
            ),
            (
                "the parent's name in another tenant's namespace",
                Some("tok-system:serviceaccount:tenant-b:ag-one"),
            ),
            (
                "the parent's tool pods",
                Some("tok-system:serviceaccount:tenant-a:ag-one-unprivileged"),
            ),
            ("a caller that is not a service account", Some("tok-ag-one")),
            ("a caller with no token", None),
        ] {
            match attach_as(&endpoint, token).await {
                Ok(_) => admitted.push(who),
                Err(e) => assert_eq!(e.code(), Code::PermissionDenied, "{who}: {e:?}"),
            }
        }
        assert!(admitted.is_empty(), "must be refused: {admitted:?}");
    }

    #[tokio::test]
    async fn the_mandate_alone_starts_the_only_turn_and_its_reply_reaches_the_attached_parent() {
        let link = link();
        let endpoint = serve(link.clone());
        let mut parent = attach_as(&endpoint, Some(PARENT_TOKEN))
            .await
            .expect("the parent attaches");
        let agent_state = Arc::new(empty_agent_state());
        let turns = ScriptedTurns::new(vec![complete_turn(StopReason::EndTurn, "the answer", &[])]);
        let mut inference = turns.clone();
        let mut relay = link.clone();
        let mut source = MandateSource::new(PATH, "summarize the repo");
        let router = test_router(agent_state.clone());
        let instructions = test_instructions();
        let run = message_loop(
            5,
            Duration::from_secs(30),
            None,
            &mut inference,
            &mut relay,
            &instructions,
            TEST_AG,
            router,
            agent_state,
            &mut source,
        );
        let drive = async {
            assert_eq!(parent.next_frame().await, final_answer("the answer"));
            let more = tokio::time::timeout(Duration::from_millis(50), parent.frames.next()).await;
            assert!(more.is_err(), "the answer is the only frame");
            let requests = turns.requests();
            assert_eq!(
                requests.len(),
                1,
                "the mandate starts one turn, and no other turn follows"
            );
            let messages: Vec<(String, String)> = requests[0]
                .messages
                .iter()
                .map(|m| (m.role.clone(), proto_common::content_text(&m.content)))
                .collect();
            assert_eq!(
                messages,
                [("user".to_string(), "summarize the repo".to_string())],
                "the first model request holds the mandate alone"
            );
        };
        tokio::select! {
            res = run => panic!("the message loop ended: {res:?}"),
            () = drive => {}
        }
    }

    #[tokio::test]
    async fn a_turn_that_ends_with_no_reply_holds_dispatch_failed_and_its_reason() {
        let link = link();
        let endpoint = serve(link.clone());
        let mut relay = link.clone();
        let _ = relay
            .deliver_turn_state(TurnStateEvent {
                state: TurnState::Failed as i32,
                reason: "model call failed".into(),
                code: "13".into(),
                ..Default::default()
            })
            .await;
        let mut parent = attach_as(&endpoint, Some(PARENT_TOKEN))
            .await
            .expect("the parent attaches");
        assert_eq!(
            parent.next_frame().await,
            final_answer("dispatch failed: model call failed")
        );
    }

    #[tokio::test]
    async fn each_attach_gets_the_held_answer_first_and_only_answer_taken_ends_the_dispatch() {
        let link = link();
        let endpoint = serve(link.clone());
        let mut relay = link.clone();
        let _ = relay
            .deliver_reply(proto_common::text_content("the answer"))
            .await;
        let ended = link.answer_taken();
        tokio::pin!(ended);

        let mut dropped = attach_as(&endpoint, Some(PARENT_TOKEN))
            .await
            .expect("the parent attaches");
        assert_eq!(dropped.next_frame().await, final_answer("the answer"));
        drop(dropped);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut ended)
                .await
                .is_err(),
            "a dropped attach must not end the dispatch"
        );

        let mut parent = attach_as(&endpoint, Some(PARENT_TOKEN))
            .await
            .expect("the parent attaches again");
        assert_eq!(parent.next_frame().await, final_answer("the answer"));
        let frame = Some(parent_frame::Frame::AnswerTaken(true));
        parent.tx.send(ParentFrame { frame }).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), ended)
            .await
            .expect("answer_taken ends the dispatch");
    }

    #[tokio::test]
    async fn an_approval_waits_for_a_parent_gets_its_answer_and_errors_when_its_attach_drops() {
        let link = link();
        let endpoint = serve(link.clone());
        let ask = |id: &'static str| {
            let mut relay = link.clone();
            tokio::spawn(async move {
                let call = relay.send_server_request_and_await(id, "RequestToolApproval", "{}", 0);
                tokio::time::timeout(Duration::from_secs(5), call).await
            })
        };
        let pending = ask("call-1");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !pending.is_finished(),
            "with no parent attached the request waits for one"
        );

        let mut parent = attach_as(&endpoint, Some(PARENT_TOKEN))
            .await
            .expect("the parent attaches");
        match parent.next_frame().await {
            Frame::Request(r) => assert_eq!(r.request_id, "call-1"),
            other => panic!("want the approval request, got {other:?}"),
        }
        let response = SendServerRequestAndAwaitResponse {
            result_json: "approved".into(),
            ..Default::default()
        };
        let frame = Some(parent_frame::Frame::Response(response));
        parent.tx.send(ParentFrame { frame }).await.unwrap();
        let outcome = pending.await.unwrap().expect("the answer ends the request");
        assert!(
            matches!(&outcome, Ok(ServerRequestOutcome::Result(r)) if r == "approved"),
            "the parent's answer comes back, got {outcome:?}"
        );

        let pending = ask("call-2");
        parent.next_frame().await;
        drop(parent);
        let outcome = pending.await.unwrap().expect("a drop ends the request");
        assert_eq!(outcome.unwrap_err(), ATTACH_DROPPED);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), ask("call-3"))
                .await
                .is_err(),
            "with its attach dropped the next request waits"
        );
    }
}
