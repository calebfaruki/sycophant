pub mod convert;
pub mod tightbeam {
    pub mod v1 {
        tonic::include_proto!("tightbeam.v1");
    }
}

pub use tightbeam::v1::*;

pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("tightbeam_descriptor");

#[cfg(test)]
mod proto_types {
    use super::*;

    #[test]
    fn message_types_exist() {
        let _msg = Message {
            role: "user".into(),
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock {
                    text: "hello".into(),
                })),
            }],
            tool_calls: vec![],
            tool_call_id: None,
            is_error: None,
            agent: None,
        };
    }

    #[test]
    fn tool_types_exist() {
        let _td = ToolDefinition {
            name: "bash".into(),
            description: "Run a command".into(),
            parameters_json: r#"{"type":"object"}"#.into(),
        };
        let _tc = ToolCall {
            id: "tc-1".into(),
            name: "bash".into(),
            input_json: r#"{"command":"ls"}"#.into(),
        };
    }

    #[test]
    fn stop_reason_variants() {
        assert_eq!(StopReason::Unspecified as i32, 0);
        assert_eq!(StopReason::EndTurn as i32, 1);
        assert_eq!(StopReason::ToolUse as i32, 2);
        assert_eq!(StopReason::MaxTokens as i32, 3);
    }

    #[test]
    fn turn_assignment_structure() {
        let _ta = TurnAssignment {
            system: Some("You are helpful.".into()),
            tools: vec![],
            messages: vec![],
        };
    }

    #[test]
    fn turn_result_chunk_variants() {
        let delta = TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::ContentDelta(ContentDelta {
                text: "Hello".into(),
            })),
        };
        assert!(matches!(
            delta.chunk,
            Some(turn_result_chunk::Chunk::ContentDelta(_))
        ));

        let tool_start = TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::ToolUseStart(ToolUseStart {
                id: "tc-1".into(),
                name: "bash".into(),
            })),
        };
        assert!(matches!(
            tool_start.chunk,
            Some(turn_result_chunk::Chunk::ToolUseStart(_))
        ));

        let complete = TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Complete(TurnComplete {
                stop_reason: StopReason::EndTurn as i32,
                content: vec![],
                tool_calls: vec![],
            })),
        };
        assert!(matches!(
            complete.chunk,
            Some(turn_result_chunk::Chunk::Complete(_))
        ));
    }

    #[test]
    fn turn_request_structure() {
        let _req = TurnRequest {
            system: Some("system prompt".into()),
            tools: vec![],
            messages: vec![],
            agent: Some("research".into()),
            model: Some("claude-sonnet".into()),
            reply_channel: None,
        };
    }

    #[test]
    fn channel_inbound_variants() {
        let reg = ChannelInbound {
            event: Some(channel_inbound::Event::Register(ChannelRegister {
                channel_type: "discord".into(),
                channel_name: "general".into(),
                workspace: Some("test-workspace".into()),
            })),
        };
        assert!(matches!(
            reg.event,
            Some(channel_inbound::Event::Register(_))
        ));

        let msg = ChannelInbound {
            event: Some(channel_inbound::Event::UserMessage(ChannelMessage {
                content: vec![],
                sender: "user123".into(),
            })),
        };
        assert!(matches!(
            msg.event,
            Some(channel_inbound::Event::UserMessage(_))
        ));
    }

    #[test]
    fn model_info_structure() {
        let _info = ModelInfo {
            name: "claude-sonnet".into(),
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            description: "Fast model".into(),
        };
    }
}
