use tightbeam_proto::{ContentBlock, UserMessage};
use tokio_stream::StreamExt;
use tonic::Streaming;

use crate::agent;

#[async_trait::async_trait]
pub(crate) trait MessageSource: Send {
    async fn next_message(&mut self) -> Result<InboundMessage, String>;
}

pub(crate) struct InboundMessage {
    pub content: Vec<ContentBlock>,
    pub sender: String,
    pub reply_channel: Option<String>,
}

pub(crate) struct StdinMessageSource {
    reader: tokio::io::BufReader<tokio::io::Stdin>,
}

impl StdinMessageSource {
    pub(crate) fn new() -> Self {
        Self {
            reader: tokio::io::BufReader::new(tokio::io::stdin()),
        }
    }
}

#[async_trait::async_trait]
impl MessageSource for StdinMessageSource {
    async fn next_message(&mut self) -> Result<InboundMessage, String> {
        use tokio::io::AsyncBufReadExt;

        let mut line = String::new();
        let bytes_read = self
            .reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("stdin read error: {e}"))?;

        if bytes_read == 0 {
            return Err("stdin closed".into());
        }

        let text = line.trim_end().to_string();
        if text.is_empty() {
            return Err("empty message".into());
        }

        Ok(InboundMessage {
            content: vec![agent::text_block(text)],
            sender: "stdin".into(),
            reply_channel: None,
        })
    }
}

pub(crate) struct SubscribeMessageSource {
    stream: Streaming<UserMessage>,
}

impl SubscribeMessageSource {
    pub(crate) fn new(stream: Streaming<UserMessage>) -> Self {
        Self { stream }
    }
}

#[async_trait::async_trait]
impl MessageSource for SubscribeMessageSource {
    async fn next_message(&mut self) -> Result<InboundMessage, String> {
        let msg = self
            .stream
            .next()
            .await
            .ok_or_else(|| "subscribe stream closed".to_string())?
            .map_err(|e| format!("subscribe stream error: {e}"))?;

        if msg.content.is_empty() {
            return Err("empty inbound message".into());
        }

        tracing::info!(sender = %msg.sender, "received inbound message");
        Ok(InboundMessage {
            content: msg.content,
            sender: msg.sender,
            reply_channel: msg.reply_channel,
        })
    }
}
