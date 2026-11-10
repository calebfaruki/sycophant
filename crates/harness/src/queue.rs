//! The harness's one in-memory FIFO of people's messages.
//!
//! In memory only: a harness restart drops it.

use std::collections::VecDeque;

use proto_common::ContentBlock;
use tokio::sync::{Mutex, Notify};

use crate::message_source::InboundMessage;

#[derive(Default)]
pub(crate) struct MessageQueue {
    items: Mutex<VecDeque<InboundMessage>>,
    ready: Notify,
}

impl MessageQueue {
    pub(crate) async fn push(&self, message: InboundMessage) {
        self.items.lock().await.push_back(message);
        self.ready.notify_one();
    }

    /// Wait until a message is queued, without taking it.
    pub(crate) async fn wait_ready(&self) {
        while self.items.lock().await.is_empty() {
            self.ready.notified().await;
        }
    }

    /// Take the oldest queued message, or `None` when a take-back emptied the
    /// queue since `wait_ready` returned.
    pub(crate) async fn pop(&self) -> Option<InboundMessage> {
        self.items.lock().await.pop_front()
    }

    /// Remove the newest queued message whose `sender` is `sender` and
    /// return its content; `None` (nothing removed) when that person has
    /// none queued. A message already taken for a turn is no longer queued,
    /// so the running turn's message is never removed.
    pub(crate) async fn take_back(&self, sender: &str) -> Option<Vec<ContentBlock>> {
        let mut items = self.items.lock().await;
        let idx = items.iter().rposition(|m| m.sender == sender)?;
        items.remove(idx).map(|m| m.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto_common::{content_text, text_content};

    const ALICE: &str = "https://issuer.example#alice";
    const BOB: &str = "https://issuer.example#bob";

    fn item(sender: &str, text: &str) -> InboundMessage {
        InboundMessage {
            sender: sender.to_string(),
            content: text_content(text),
            reply_channel_stream: None,
            message_id: format!("mid-{text}"),
        }
    }

    fn taken(content: Option<Vec<ContentBlock>>) -> Option<String> {
        content.map(|c| content_text(&c))
    }

    // A take-back removes the caller's newest queued message and returns its
    // content. Another person's queued message and the message of the turn
    // already running are never removed, so with nothing of the caller's
    // left queued it takes nothing.
    #[tokio::test]
    async fn take_back_only_removes_the_callers_newest_queued_message() {
        let queue = MessageQueue::default();
        queue.push(item(ALICE, "A0")).await;
        let running = queue.pop().await.unwrap();
        assert_eq!(content_text(&running.content), "A0");

        queue.push(item(ALICE, "A1")).await;
        queue.push(item(BOB, "B1")).await;
        queue.push(item(ALICE, "A2")).await;

        assert_eq!(
            taken(queue.take_back(ALICE).await).as_deref(),
            Some("A2"),
            "the first take-back removes alice's newest queued message"
        );
        assert_eq!(
            taken(queue.take_back(ALICE).await).as_deref(),
            Some("A1"),
            "the next take-back removes her next newest"
        );
        assert_eq!(
            taken(queue.take_back(ALICE).await),
            None,
            "bob's message and the running turn's message are not alice's to take back"
        );

        let next = queue.pop().await.unwrap();
        assert_eq!(next.sender, BOB);
        assert_eq!(
            content_text(&next.content),
            "B1",
            "bob's message is still queued"
        );
    }
}
