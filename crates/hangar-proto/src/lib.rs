pub mod convert;
pub mod hangar {
    pub mod v1 {
        tonic::include_proto!("hangar.v1");
    }
}

pub use hangar::v1::*;
pub use proto_common::*;
pub use transponder_proto::{transponder_control_client, transponder_control_server};

pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("hangar_descriptor");

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
    fn turn_warning_constructs_with_field_and_reason() {
        let w = TurnWarning {
            field: "model".into(),
            reason: "operator binds the model identifier to the API key".into(),
        };
        assert_eq!(w.field, "model");
        assert_eq!(
            w.reason,
            "operator binds the model identifier to the API key"
        );
    }

    #[test]
    fn turn_assignment_carries_optional_params_json() {
        let assignment = TurnAssignment {
            system: None,
            tools: vec![],
            messages: vec![],
            params_json: Some(r#"{"output_config":{"effort":"high"}}"#.into()),
        };
        assert_eq!(
            assignment.params_json.as_deref(),
            Some(r#"{"output_config":{"effort":"high"}}"#)
        );
    }

    #[test]
    fn turn_result_chunk_warning_variant_constructs() {
        let chunk = TurnResultChunk {
            chunk: Some(turn_result_chunk::Chunk::Warning(TurnWarning {
                field: "messages".into(),
                reason: "managed".into(),
            })),
        };
        assert!(matches!(
            chunk.chunk,
            Some(turn_result_chunk::Chunk::Warning(_))
        ));
    }

    #[test]
    fn turn_event_warning_variant_constructs() {
        let event = TurnEvent {
            event: Some(turn_event::Event::Warning(TurnWarning {
                field: "tools".into(),
                reason: "managed".into(),
            })),
        };
        assert!(matches!(event.event, Some(turn_event::Event::Warning(_))));
    }
}
