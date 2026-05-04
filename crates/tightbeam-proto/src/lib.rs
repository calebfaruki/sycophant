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
    fn stop_reason_variants() {
        assert_eq!(StopReason::Unspecified as i32, 0);
        assert_eq!(StopReason::EndTurn as i32, 1);
        assert_eq!(StopReason::ToolUse as i32, 2);
        assert_eq!(StopReason::MaxTokens as i32, 3);
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
            event: Some(channel_inbound::Event::UserMessage(UserMessage {
                content: vec![],
                sender: "user123".into(),
                reply_channel: None,
            })),
        };
        assert!(matches!(
            msg.event,
            Some(channel_inbound::Event::UserMessage(_))
        ));
    }
}
