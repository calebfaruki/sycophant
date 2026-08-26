use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use proto_common::{ContentBlock, UserMessage};
use tokio_stream::StreamExt;
use tonic::Streaming;

use crate::clients::RelayClient;

#[async_trait::async_trait]
pub(crate) trait MessageSource: Send {
    async fn next_message(&mut self) -> Result<InboundMessage, String>;
}

#[derive(Debug)]
pub(crate) struct InboundMessage {
    pub content: Vec<ContentBlock>,
    pub reply_channel: Option<String>,
    /// Conversation this message belongs to. Stamped at ingest time.
    /// The harness uses this verbatim when building the TurnRequest;
    /// it never mints conversation ids on its own.
    pub conversation_id: String,
    /// The message's human-selected credential grants, keyed by toolset name.
    /// A toolset selected twice keeps the last selection.
    pub grants: std::collections::HashMap<String, String>,
}

/// Abstraction over "subscribe to a UserMessage stream and pull the next
/// message" so the reconnect loop in `SubscribeMessageSource` can be unit-tested
/// without standing up a tonic server. `subscribe` is idempotent and may be
/// called repeatedly to (re)establish the stream.
#[async_trait::async_trait]
pub(crate) trait SubscribeDriver: Send {
    async fn subscribe(&mut self) -> Result<(), String>;
    async fn next(&mut self) -> Result<UserMessage, String>;
}

pub(crate) struct RelayDriver {
    client: RelayClient,
    stream: Option<Streaming<UserMessage>>,
}

impl RelayDriver {
    pub(crate) fn new(client: RelayClient) -> Self {
        Self {
            client,
            stream: None,
        }
    }
}

#[async_trait::async_trait]
impl SubscribeDriver for RelayDriver {
    async fn subscribe(&mut self) -> Result<(), String> {
        self.stream = Some(self.client.subscribe().await?);
        Ok(())
    }

    async fn next(&mut self) -> Result<UserMessage, String> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| "not subscribed".to_string())?;
        stream
            .next()
            .await
            .ok_or_else(|| "subscribe stream closed".to_string())?
            .map_err(|e| format!("subscribe stream error: {e}"))
    }
}

pub(crate) struct SubscribeMessageSource {
    driver: Box<dyn SubscribeDriver>,
    subscribed_flag: Arc<AtomicBool>,
    backoff: Duration,
}

impl SubscribeMessageSource {
    pub(crate) fn from_client(client: RelayClient, subscribed_flag: Arc<AtomicBool>) -> Self {
        Self::with_driver(Box::new(RelayDriver::new(client)), subscribed_flag)
    }

    fn with_driver(driver: Box<dyn SubscribeDriver>, subscribed_flag: Arc<AtomicBool>) -> Self {
        Self {
            driver,
            subscribed_flag,
            backoff: Duration::from_secs(2),
        }
    }

    #[cfg(test)]
    fn no_backoff(mut self) -> Self {
        self.backoff = Duration::ZERO;
        self
    }
}

#[async_trait::async_trait]
impl MessageSource for SubscribeMessageSource {
    async fn next_message(&mut self) -> Result<InboundMessage, String> {
        loop {
            if !self.subscribed_flag.load(Ordering::Relaxed) {
                match self.driver.subscribe().await {
                    Ok(()) => {
                        self.subscribed_flag.store(true, Ordering::Relaxed);
                        tracing::info!("subscribed to relay gateway for inbound messages");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "subscribe failed, retrying after backoff");
                        tokio::time::sleep(self.backoff).await;
                        continue;
                    }
                }
            }

            match self.driver.next().await {
                Ok(msg) => {
                    if msg.content.is_empty() {
                        return Err("empty inbound message".into());
                    }
                    tracing::info!(
                        sender = %msg.sender,
                        conversation_id = %msg.conversation_id,
                        "received inbound message"
                    );
                    return Ok(InboundMessage {
                        content: msg.content,
                        reply_channel: msg.reply_channel,
                        conversation_id: msg.conversation_id,
                        grants: msg
                            .grants
                            .into_iter()
                            .map(|g| (g.toolset, g.grant))
                            .collect(),
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "subscribe stream broke, will reconnect");
                    self.subscribed_flag.store(false, Ordering::Relaxed);
                    tokio::time::sleep(self.backoff).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto_common::{content_block, TextBlock};
    use std::collections::VecDeque;

    /// Mirrors the production `RelayDriver` invariants so the reconnect
    /// loop's `subscribed` gate is actually exercised: `next()` errors when
    /// not subscribed, and a failed `next()` invalidates the subscription.
    struct MockDriver {
        subscribe_queue: VecDeque<Result<(), String>>,
        next_queue: VecDeque<Result<UserMessage, String>>,
        subscribed: bool,
    }

    impl MockDriver {
        fn new(subscribe: Vec<Result<(), String>>, next: Vec<Result<UserMessage, String>>) -> Self {
            Self {
                subscribe_queue: subscribe.into(),
                next_queue: next.into(),
                subscribed: false,
            }
        }
    }

    #[async_trait::async_trait]
    impl SubscribeDriver for MockDriver {
        async fn subscribe(&mut self) -> Result<(), String> {
            let result = self
                .subscribe_queue
                .pop_front()
                .expect("test exhausted subscribe queue");
            if result.is_ok() {
                self.subscribed = true;
            }
            result
        }

        async fn next(&mut self) -> Result<UserMessage, String> {
            if !self.subscribed {
                return Err("not subscribed".into());
            }
            let result = self
                .next_queue
                .pop_front()
                .expect("test exhausted next queue");
            if result.is_err() {
                self.subscribed = false;
            }
            result
        }
    }

    fn user_message(text: &str) -> UserMessage {
        UserMessage {
            sender: "u".into(),
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
            }],
            reply_channel: None,
            conversation_id: "test-conv".into(),
            grants: vec![],
        }
    }

    #[tokio::test]
    async fn subscribe_message_source_happy_path_yields_message() {
        let driver = MockDriver::new(vec![Ok(())], vec![Ok(user_message("hi"))]);
        let flag = Arc::new(AtomicBool::new(false));
        let mut src =
            SubscribeMessageSource::with_driver(Box::new(driver), flag.clone()).no_backoff();
        let msg = src.next_message().await.unwrap();
        assert_eq!(msg.content.len(), 1);
        assert_eq!(msg.conversation_id, "test-conv");
        assert!(flag.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn subscribe_message_source_reconnects_after_stream_error() {
        let driver = MockDriver::new(
            vec![Ok(()), Ok(())],
            vec![
                Err("stream broke".into()),
                Ok(user_message("after-reconnect")),
            ],
        );
        let flag = Arc::new(AtomicBool::new(false));
        let mut src = SubscribeMessageSource::with_driver(Box::new(driver), flag).no_backoff();
        let msg = src.next_message().await.unwrap();
        let text = match &msg.content[0].block {
            Some(content_block::Block::Text(t)) => t.text.clone(),
            _ => panic!("expected text block"),
        };
        assert_eq!(text, "after-reconnect");
    }

    #[tokio::test]
    async fn subscribe_message_source_loops_on_persistent_subscribe_failure() {
        let mut subscribe = vec![Err::<(), String>("subscribe failed".into()); 5];
        subscribe.push(Ok(()));
        let driver = MockDriver::new(subscribe, vec![Ok(user_message("eventually"))]);
        let flag = Arc::new(AtomicBool::new(false));
        let mut src = SubscribeMessageSource::with_driver(Box::new(driver), flag).no_backoff();
        let msg = src.next_message().await.unwrap();
        assert_eq!(msg.content.len(), 1);
    }

    #[tokio::test]
    async fn subscribe_message_source_returns_err_on_empty_content() {
        let driver = MockDriver::new(
            vec![Ok(())],
            vec![Ok(UserMessage {
                sender: "u".into(),
                content: vec![],
                reply_channel: None,
                conversation_id: String::new(),
                grants: vec![],
            })],
        );
        let flag = Arc::new(AtomicBool::new(false));
        let mut src = SubscribeMessageSource::with_driver(Box::new(driver), flag).no_backoff();
        let err = src.next_message().await.unwrap_err();
        assert_eq!(err, "empty inbound message");
    }

    #[tokio::test]
    async fn subscribe_message_source_reconnects_after_subscribe_then_stream_error() {
        let driver = MockDriver::new(
            vec![Ok(()), Err("connect failed".into()), Ok(())],
            vec![Err("stream broke".into()), Ok(user_message("after-retry"))],
        );
        let flag = Arc::new(AtomicBool::new(false));
        let mut src = SubscribeMessageSource::with_driver(Box::new(driver), flag).no_backoff();
        let msg = src.next_message().await.unwrap();
        assert_eq!(msg.content.len(), 1);
    }
}
