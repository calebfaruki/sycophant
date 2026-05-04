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
        let text = read_one_line(&mut self.reader).await?;
        Ok(InboundMessage {
            content: vec![agent::text_block(text)],
            reply_channel: None,
        })
    }
}

/// Read a single non-empty line from any async buffered reader.
///
/// Returns `Err("stdin closed")` on EOF (`bytes_read == 0`) and `Err("empty
/// message")` on a line that is whitespace-only after trimming. Separated from
/// `StdinMessageSource` so the EOF / empty-line decisions are unit-testable
/// without piping a real stdin.
async fn read_one_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<String, String> {
    use tokio::io::AsyncBufReadExt;

    let mut line = String::new();
    let bytes_read = reader
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

    Ok(text)
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
            reply_channel: msg.reply_channel,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_one_line_returns_stdin_closed_on_eof() {
        let empty: &[u8] = b"";
        let mut reader = tokio::io::BufReader::new(empty);
        let err = read_one_line(&mut reader).await.unwrap_err();
        assert_eq!(err, "stdin closed");
    }

    #[tokio::test]
    async fn read_one_line_returns_empty_message_on_whitespace_line() {
        let input: &[u8] = b"\n";
        let mut reader = tokio::io::BufReader::new(input);
        let err = read_one_line(&mut reader).await.unwrap_err();
        assert_eq!(err, "empty message");
    }

    #[tokio::test]
    async fn read_one_line_returns_text_on_normal_line() {
        let input: &[u8] = b"hello world\n";
        let mut reader = tokio::io::BufReader::new(input);
        let text = read_one_line(&mut reader).await.unwrap();
        assert_eq!(text, "hello world");
    }
}
