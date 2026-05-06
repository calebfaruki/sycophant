use crate::types::{Message, ToolDefinition};
use crate::{LlmProvider, ProviderConfig, StreamEvent};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

pub struct GeminiProvider {
    // Held for constructor-signature parity with ClaudeProvider/OpenAiProvider
    // so a future real implementation lands without changing Format::build.
    #[allow(dead_code)]
    base_url: String,
}

impl GeminiProvider {
    pub fn new(base_url: String) -> Self {
        Self { base_url }
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    async fn call(
        &self,
        _messages: &[Message],
        _system: Option<&str>,
        _tools: &[ToolDefinition],
        _params: Option<&serde_json::Map<String, serde_json::Value>>,
        _config: &ProviderConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, String>> + Send>>, String> {
        Err("gemini format not yet implemented".into())
    }

    fn managed_fields(&self) -> &'static [&'static str] {
        &["model", "contents", "systemInstruction", "tools"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderConfig;

    fn cfg() -> ProviderConfig {
        ProviderConfig {
            model: "gemini-2.5-pro".into(),
            api_key: "test".into(),
        }
    }

    #[test]
    fn gemini_call_returns_not_implemented_error() {
        let provider = GeminiProvider::new("https://generativelanguage.googleapis.com".into());
        let result = futures::executor::block_on(provider.call(&[], None, &[], None, &cfg()));
        match result {
            Err(e) => assert_eq!(e, "gemini format not yet implemented"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
    }
}
