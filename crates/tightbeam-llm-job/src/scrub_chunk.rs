use shared::scrub::ScrubSet;
use tightbeam_proto::content_block::Block;
use tightbeam_proto::turn_result_chunk::Chunk;
use tightbeam_proto::TurnResultChunk;

pub(crate) fn scrub_chunk(chunk: &mut TurnResultChunk, set: &ScrubSet) {
    if set.is_empty() {
        return;
    }
    match &mut chunk.chunk {
        Some(Chunk::ContentDelta(delta)) => {
            delta.text = set.apply(&delta.text);
        }
        Some(Chunk::Error(err)) => {
            err.message = set.apply(&err.message);
        }
        Some(Chunk::Complete(complete)) => {
            for cb in &mut complete.content {
                if let Some(block) = &mut cb.block {
                    match block {
                        Block::Text(t) => t.text = set.apply(&t.text),
                        Block::Thinking(t) => t.text = set.apply(&t.text),
                        Block::Image(_) => {}
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tightbeam_proto::{
        content_block, ContentBlock, ContentDelta, StopReason, TextBlock, ThinkingBlock,
        ToolUseInput, ToolUseStart, TurnComplete, TurnError, TurnResultChunk,
    };

    const SCRUB_ENV: &str = "TEST_TIGHTBEAM_SCRUB";

    fn with_secret<F: FnOnce(ScrubSet)>(secret: &str, name: &str, f: F) {
        std::env::set_var("TEST_API_KEY", secret);
        std::env::set_var(
            SCRUB_ENV,
            format!(r#"[{{"name":"{name}","env":"TEST_API_KEY"}}]"#),
        );
        let set = ScrubSet::from_env_var(SCRUB_ENV);
        f(set);
        std::env::remove_var("TEST_API_KEY");
        std::env::remove_var(SCRUB_ENV);
    }

    fn make_error_chunk(message: &str) -> TurnResultChunk {
        TurnResultChunk {
            chunk: Some(Chunk::Error(TurnError {
                code: -1,
                message: message.into(),
            })),
        }
    }

    fn make_content_delta(text: &str) -> TurnResultChunk {
        TurnResultChunk {
            chunk: Some(Chunk::ContentDelta(ContentDelta { text: text.into() })),
        }
    }

    fn make_complete(text: &str, thinking: &str) -> TurnResultChunk {
        TurnResultChunk {
            chunk: Some(Chunk::Complete(TurnComplete {
                stop_reason: StopReason::EndTurn as i32,
                content: vec![
                    ContentBlock {
                        block: Some(content_block::Block::Thinking(ThinkingBlock {
                            text: thinking.into(),
                        })),
                    },
                    ContentBlock {
                        block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
                    },
                ],
                tool_calls: vec![],
            })),
        }
    }

    #[test]
    #[serial]
    fn error_chunk_message_scrubbed() {
        with_secret("sk-leak-key-12345", "api", |set| {
            let mut chunk = make_error_chunk("401: invalid auth: Bearer sk-leak-key-12345");
            scrub_chunk(&mut chunk, &set);
            match chunk.chunk.unwrap() {
                Chunk::Error(err) => {
                    assert!(err.message.contains("[REDACTED:api]"));
                    assert!(
                        !err.message.contains("sk-leak-key-12345"),
                        "raw key bytes must not appear: {}",
                        err.message
                    );
                }
                _ => panic!("expected Error chunk"),
            }
        });
    }

    #[test]
    #[serial]
    fn content_delta_text_scrubbed() {
        with_secret("sk-leak-key-54321", "api", |set| {
            let mut chunk = make_content_delta("the model said sk-leak-key-54321 verbatim");
            scrub_chunk(&mut chunk, &set);
            match chunk.chunk.unwrap() {
                Chunk::ContentDelta(d) => {
                    assert!(d.text.contains("[REDACTED:api]"));
                    assert!(!d.text.contains("sk-leak-key-54321"));
                }
                _ => panic!("expected ContentDelta"),
            }
        });
    }

    #[test]
    #[serial]
    fn complete_textblock_and_thinkingblock_scrubbed() {
        with_secret("sk-leak-key-finale", "api", |set| {
            let mut chunk = make_complete(
                "text says sk-leak-key-finale",
                "thoughts: sk-leak-key-finale",
            );
            scrub_chunk(&mut chunk, &set);
            match chunk.chunk.unwrap() {
                Chunk::Complete(complete) => {
                    for cb in &complete.content {
                        match cb.block.as_ref().unwrap() {
                            content_block::Block::Text(t) => {
                                assert!(t.text.contains("[REDACTED:api]"));
                                assert!(!t.text.contains("sk-leak-key-finale"));
                            }
                            content_block::Block::Thinking(t) => {
                                assert!(t.text.contains("[REDACTED:api]"));
                                assert!(!t.text.contains("sk-leak-key-finale"));
                            }
                            content_block::Block::Image(_) => {}
                        }
                    }
                }
                _ => panic!("expected Complete chunk"),
            }
        });
    }

    #[test]
    #[serial]
    fn tool_use_input_intentionally_not_scrubbed() {
        // ToolUseInput is LLM-generated tool args; the LLM doesn't see the
        // api_key, so it cannot leak it here. We deliberately do NOT scrub
        // model-generated tool input to avoid corrupting legitimate JSON.
        with_secret("sk-leak-key", "api", |set| {
            let mut chunk = TurnResultChunk {
                chunk: Some(Chunk::ToolUseInput(ToolUseInput {
                    partial_json: "{\"key\":\"sk-leak-key\"}".into(),
                })),
            };
            scrub_chunk(&mut chunk, &set);
            match chunk.chunk.unwrap() {
                Chunk::ToolUseInput(t) => assert!(t.partial_json.contains("sk-leak-key")),
                _ => panic!("expected ToolUseInput"),
            }
        });
    }

    #[test]
    #[serial]
    fn empty_scrubset_is_noop_on_error_chunk() {
        std::env::remove_var(SCRUB_ENV);
        let set = ScrubSet::from_env_var(SCRUB_ENV);
        let mut chunk = make_error_chunk("sensitive: sk-something");
        scrub_chunk(&mut chunk, &set);
        match chunk.chunk.unwrap() {
            Chunk::Error(err) => assert_eq!(err.message, "sensitive: sk-something"),
            _ => panic!(),
        }
    }

    #[test]
    #[serial]
    fn tool_use_start_not_scrubbed() {
        with_secret("sk-leak-key", "api", |set| {
            let mut chunk = TurnResultChunk {
                chunk: Some(Chunk::ToolUseStart(ToolUseStart {
                    id: "id-1".into(),
                    name: "tool".into(),
                })),
            };
            scrub_chunk(&mut chunk, &set);
            // No panic, no change — ToolUseStart fields aren't scrubbed.
            matches!(chunk.chunk.unwrap(), Chunk::ToolUseStart(_));
        });
    }
}
