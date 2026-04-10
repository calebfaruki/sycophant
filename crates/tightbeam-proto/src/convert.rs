use crate as proto;
use tightbeam_providers::types as provider;

pub fn provider_message_to_proto(msg: &provider::Message) -> proto::Message {
    let content = match &msg.content {
        Some(blocks) => blocks.iter().map(provider_content_to_proto).collect(),
        None => vec![],
    };

    let tool_calls = match &msg.tool_calls {
        Some(calls) => calls.iter().map(provider_tool_call_to_proto).collect(),
        None => vec![],
    };

    proto::Message {
        role: msg.role.clone(),
        content,
        tool_calls,
        tool_call_id: msg.tool_call_id.clone(),
        is_error: msg.is_error,
        agent: msg.agent.clone(),
    }
}

pub fn proto_message_to_provider(msg: &proto::Message) -> provider::Message {
    let content = if msg.content.is_empty() {
        None
    } else {
        Some(
            msg.content
                .iter()
                .filter_map(proto_content_to_provider)
                .collect(),
        )
    };

    let tool_calls = if msg.tool_calls.is_empty() {
        None
    } else {
        Some(
            msg.tool_calls
                .iter()
                .map(proto_tool_call_to_provider)
                .collect(),
        )
    };

    provider::Message {
        role: msg.role.clone(),
        content,
        tool_calls,
        tool_call_id: msg.tool_call_id.clone(),
        is_error: msg.is_error,
        agent: msg.agent.clone(),
    }
}

pub fn provider_content_to_proto(block: &provider::ContentBlock) -> proto::ContentBlock {
    match block {
        provider::ContentBlock::Text { text } => proto::ContentBlock {
            block: Some(proto::content_block::Block::Text(proto::TextBlock {
                text: text.clone(),
            })),
        },
        provider::ContentBlock::Image { media_type, data } => {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap_or_else(|_| data.as_bytes().to_vec());
            proto::ContentBlock {
                block: Some(proto::content_block::Block::Image(proto::ImageBlock {
                    media_type: media_type.clone(),
                    data: bytes,
                })),
            }
        }
        provider::ContentBlock::Thinking { text } => proto::ContentBlock {
            block: Some(proto::content_block::Block::Thinking(
                proto::ThinkingBlock { text: text.clone() },
            )),
        },
        provider::ContentBlock::FileIncoming { .. } => proto::ContentBlock { block: None },
    }
}

pub fn proto_content_to_provider(block: &proto::ContentBlock) -> Option<provider::ContentBlock> {
    match &block.block {
        Some(proto::content_block::Block::Text(t)) => Some(provider::ContentBlock::Text {
            text: t.text.clone(),
        }),
        Some(proto::content_block::Block::Image(img)) => {
            use base64::Engine;
            let data = base64::engine::general_purpose::STANDARD.encode(&img.data);
            Some(provider::ContentBlock::Image {
                media_type: img.media_type.clone(),
                data,
            })
        }
        Some(proto::content_block::Block::Thinking(t)) => Some(provider::ContentBlock::Thinking {
            text: t.text.clone(),
        }),
        None => None,
    }
}

pub fn provider_tool_call_to_proto(tc: &provider::ToolCall) -> proto::ToolCall {
    proto::ToolCall {
        id: tc.id.clone(),
        name: tc.name.clone(),
        input_json: serde_json::to_string(&tc.input).unwrap_or_default(),
    }
}

pub fn proto_tool_call_to_provider(tc: &proto::ToolCall) -> provider::ToolCall {
    let input = serde_json::from_str(&tc.input_json).unwrap_or(serde_json::Value::Null);
    provider::ToolCall {
        id: tc.id.clone(),
        name: tc.name.clone(),
        input,
    }
}

pub fn provider_stop_reason_to_proto(sr: &provider::StopReason) -> i32 {
    match sr {
        provider::StopReason::EndTurn => proto::StopReason::EndTurn as i32,
        provider::StopReason::ToolUse => proto::StopReason::ToolUse as i32,
        provider::StopReason::MaxTokens => proto::StopReason::MaxTokens as i32,
    }
}

pub fn proto_stop_reason_to_provider(sr: i32) -> provider::StopReason {
    match proto::StopReason::try_from(sr) {
        Ok(proto::StopReason::EndTurn) => provider::StopReason::EndTurn,
        Ok(proto::StopReason::ToolUse) => provider::StopReason::ToolUse,
        Ok(proto::StopReason::MaxTokens) => provider::StopReason::MaxTokens,
        _ => provider::StopReason::EndTurn,
    }
}

pub fn stream_event_to_chunk(event: &tightbeam_providers::StreamEvent) -> proto::TurnResultChunk {
    match event {
        tightbeam_providers::StreamEvent::ContentDelta { text } => proto::TurnResultChunk {
            chunk: Some(proto::turn_result_chunk::Chunk::ContentDelta(
                proto::ContentDelta { text: text.clone() },
            )),
        },
        tightbeam_providers::StreamEvent::ToolUseStart { id, name } => proto::TurnResultChunk {
            chunk: Some(proto::turn_result_chunk::Chunk::ToolUseStart(
                proto::ToolUseStart {
                    id: id.clone(),
                    name: name.clone(),
                },
            )),
        },
        tightbeam_providers::StreamEvent::ToolUseInput { json } => proto::TurnResultChunk {
            chunk: Some(proto::turn_result_chunk::Chunk::ToolUseInput(
                proto::ToolUseInput {
                    partial_json: json.clone(),
                },
            )),
        },
        tightbeam_providers::StreamEvent::ThinkingDelta { .. } => {
            // Thinking deltas are accumulated by the LLM Job, not streamed.
            proto::TurnResultChunk { chunk: None }
        }
        tightbeam_providers::StreamEvent::Done { stop_reason } => {
            let sr = provider::StopReason::from_str_lossy(stop_reason);
            proto::TurnResultChunk {
                chunk: Some(proto::turn_result_chunk::Chunk::Complete(
                    proto::TurnComplete {
                        stop_reason: provider_stop_reason_to_proto(&sr),
                        content: vec![],
                        tool_calls: vec![],
                    },
                )),
            }
        }
    }
}

pub fn chunk_to_turn_event(chunk: proto::TurnResultChunk) -> proto::TurnEvent {
    proto::TurnEvent {
        event: match chunk.chunk {
            Some(proto::turn_result_chunk::Chunk::ContentDelta(d)) => {
                Some(proto::turn_event::Event::ContentDelta(d))
            }
            Some(proto::turn_result_chunk::Chunk::ToolUseStart(t)) => {
                Some(proto::turn_event::Event::ToolUseStart(t))
            }
            Some(proto::turn_result_chunk::Chunk::ToolUseInput(i)) => {
                Some(proto::turn_event::Event::ToolUseInput(i))
            }
            Some(proto::turn_result_chunk::Chunk::Complete(c)) => {
                Some(proto::turn_event::Event::Complete(c))
            }
            Some(proto::turn_result_chunk::Chunk::Error(e)) => {
                Some(proto::turn_event::Event::Error(e))
            }
            None => None,
        },
    }
}

pub fn proto_tool_def_to_provider(td: &proto::ToolDefinition) -> provider::ToolDefinition {
    let parameters = serde_json::from_str(&td.parameters_json).unwrap_or(serde_json::Value::Null);
    provider::ToolDefinition {
        name: td.name.clone(),
        description: td.description.clone(),
        parameters,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_message_round_trips() {
        let orig = provider::Message {
            role: "user".into(),
            content: Some(provider::ContentBlock::text_content("hello")),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
            agent: None,
        };
        let proto_msg = provider_message_to_proto(&orig);
        let back = proto_message_to_provider(&proto_msg);
        assert_eq!(back.role, "user");
        assert_eq!(provider::content_text(&back.content), Some("hello"));
        assert!(back.tool_calls.is_none());
    }

    #[test]
    fn tool_call_json_round_trips() {
        let tc = provider::ToolCall {
            id: "tc-1".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
        };
        let proto_tc = provider_tool_call_to_proto(&tc);
        assert_eq!(proto_tc.input_json, r#"{"command":"ls"}"#);
        let back = proto_tool_call_to_provider(&proto_tc);
        assert_eq!(back.input, serde_json::json!({"command": "ls"}));
    }

    #[test]
    fn empty_content_becomes_none() {
        let proto_msg = proto::Message {
            role: "assistant".into(),
            content: vec![],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
            agent: None,
        };
        let msg = proto_message_to_provider(&proto_msg);
        assert!(msg.content.is_none());
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn none_content_becomes_empty_vec() {
        let msg = provider::Message {
            role: "assistant".into(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
            agent: None,
        };
        let proto_msg = provider_message_to_proto(&msg);
        assert!(proto_msg.content.is_empty());
        assert!(proto_msg.tool_calls.is_empty());
    }

    #[test]
    fn agent_field_preserved() {
        let msg = provider::Message {
            role: "assistant".into(),
            content: Some(provider::ContentBlock::text_content("hi")),
            tool_calls: None,
            tool_call_id: None,
            is_error: None,
            agent: Some("research".into()),
        };
        let proto_msg = provider_message_to_proto(&msg);
        assert_eq!(proto_msg.agent, Some("research".into()));
        let back = proto_message_to_provider(&proto_msg);
        assert_eq!(back.agent, Some("research".into()));
    }

    #[test]
    fn stop_reason_round_trips_all_variants() {
        assert_eq!(
            provider_stop_reason_to_proto(&provider::StopReason::EndTurn),
            proto::StopReason::EndTurn as i32
        );
        assert_eq!(
            provider_stop_reason_to_proto(&provider::StopReason::ToolUse),
            proto::StopReason::ToolUse as i32
        );
        assert_eq!(
            provider_stop_reason_to_proto(&provider::StopReason::MaxTokens),
            proto::StopReason::MaxTokens as i32
        );
        assert!(matches!(
            proto_stop_reason_to_provider(proto::StopReason::EndTurn as i32),
            provider::StopReason::EndTurn
        ));
        assert!(matches!(
            proto_stop_reason_to_provider(proto::StopReason::ToolUse as i32),
            provider::StopReason::ToolUse
        ));
        assert!(matches!(
            proto_stop_reason_to_provider(proto::StopReason::MaxTokens as i32),
            provider::StopReason::MaxTokens
        ));
    }

    #[test]
    fn stream_event_content_delta_converts() {
        let event = tightbeam_providers::StreamEvent::ContentDelta {
            text: "Hello".into(),
        };
        let chunk = stream_event_to_chunk(&event);
        assert!(matches!(
            chunk.chunk,
            Some(proto::turn_result_chunk::Chunk::ContentDelta(_))
        ));
    }

    #[test]
    fn stream_event_done_converts() {
        let event = tightbeam_providers::StreamEvent::Done {
            stop_reason: "end_turn".into(),
        };
        let chunk = stream_event_to_chunk(&event);
        match chunk.chunk.unwrap() {
            proto::turn_result_chunk::Chunk::Complete(c) => {
                assert_eq!(c.stop_reason, proto::StopReason::EndTurn as i32);
            }
            _ => panic!("expected Complete"),
        }
    }

    #[test]
    fn invalid_json_input_becomes_null() {
        let proto_tc = proto::ToolCall {
            id: "tc-1".into(),
            name: "bash".into(),
            input_json: "not valid json{".into(),
        };
        let provider_tc = proto_tool_call_to_provider(&proto_tc);
        assert!(provider_tc.input.is_null());
    }

    #[test]
    fn tool_def_round_trips() {
        let td = proto::ToolDefinition {
            name: "bash".into(),
            description: "Run a command".into(),
            parameters_json: r#"{"type":"object"}"#.into(),
        };
        let back = proto_tool_def_to_provider(&td);
        assert_eq!(back.name, "bash");
        assert_eq!(back.parameters, serde_json::json!({"type": "object"}));
    }

    #[test]
    fn chunk_to_turn_event_maps_all_variants() {
        let delta = chunk_to_turn_event(proto::TurnResultChunk {
            chunk: Some(proto::turn_result_chunk::Chunk::ContentDelta(
                proto::ContentDelta { text: "hi".into() },
            )),
        });
        assert!(matches!(
            delta.event,
            Some(proto::turn_event::Event::ContentDelta(_))
        ));

        let none = chunk_to_turn_event(proto::TurnResultChunk { chunk: None });
        assert!(none.event.is_none());
    }
}
