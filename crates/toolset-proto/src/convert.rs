use crate as proto;

/// Convert a worker-produced turn-result chunk into the harness-facing turn
/// event. A pure proto→proto shape map: the two oneofs carry the same arms,
/// the worker surface names them `TurnResultChunk`, the harness surface names
/// them `TurnEvent`.
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
            Some(proto::turn_result_chunk::Chunk::Warning(w)) => {
                Some(proto::turn_event::Event::Warning(w))
            }
            None => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_warning_converts_to_event_warning() {
        let chunk = proto::TurnResultChunk {
            chunk: Some(proto::turn_result_chunk::Chunk::Warning(
                proto::TurnWarning {
                    field: "model".into(),
                    reason: "operator-bound".into(),
                },
            )),
        };
        let event = chunk_to_turn_event(chunk);
        match event.event {
            Some(proto::turn_event::Event::Warning(w)) => {
                assert_eq!(w.field, "model");
                assert_eq!(w.reason, "operator-bound");
            }
            other => panic!("expected Warning, got {other:?}"),
        }
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
