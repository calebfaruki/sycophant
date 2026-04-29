use tightbeam_proto::{ContentBlock, InboundMessage};
use tokio_stream::StreamExt;
use tonic::Streaming;

use crate::agent;

#[async_trait::async_trait]
pub(crate) trait MessageSource: Send {
    async fn next_message(&mut self) -> Result<(Vec<ContentBlock>, Option<String>), String>;
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
    async fn next_message(&mut self) -> Result<(Vec<ContentBlock>, Option<String>), String> {
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

        Ok((vec![agent::text_block(text)], None))
    }
}

pub(crate) struct SubscribeMessageSource {
    stream: Streaming<InboundMessage>,
}

impl SubscribeMessageSource {
    pub(crate) fn new(stream: Streaming<InboundMessage>) -> Self {
        Self { stream }
    }
}

#[async_trait::async_trait]
impl MessageSource for SubscribeMessageSource {
    async fn next_message(&mut self) -> Result<(Vec<ContentBlock>, Option<String>), String> {
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
        Ok((msg.content, msg.reply_channel))
    }
}
