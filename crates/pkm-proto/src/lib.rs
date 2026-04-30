pub mod pkm {
    pub mod v1 {
        tonic::include_proto!("pkm.v1");
    }
}

pub use pkm::v1::*;

pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("pkm_descriptor");

#[cfg(test)]
mod proto_types {
    use super::*;

    #[test]
    fn transponder_event_variants() {
        let user = TransponderEvent {
            event: Some(transponder_event::Event::UserMessage(UserMessage {
                content: vec![],
                sender: "alice".into(),
            })),
        };
        assert!(matches!(
            user.event,
            Some(transponder_event::Event::UserMessage(_))
        ));

        let report = TransponderEvent {
            event: Some(transponder_event::Event::ReportSystemTurn(
                ReportSystemTurn {
                    response_json: r#"{"agent_name":"research"}"#.into(),
                    structured_json: None,
                },
            )),
        };
        assert!(matches!(
            report.event,
            Some(transponder_event::Event::ReportSystemTurn(_))
        ));
    }

    #[test]
    fn pkm_event_variants() {
        let run_sys = PkmEvent {
            event: Some(pkm_event::Event::RunSystemTurn(RunSystemTurn {
                system_prompt: "x".into(),
                messages: vec![],
                response_schema_json: None,
            })),
        };
        assert!(matches!(
            run_sys.event,
            Some(pkm_event::Event::RunSystemTurn(_))
        ));

        let run_agent = PkmEvent {
            event: Some(pkm_event::Event::RunAgentTurn(RunAgentTurn {
                agent_name: "research".into(),
                system_prompt: "x".into(),
                system_messages: vec![],
            })),
        };
        assert!(matches!(
            run_agent.event,
            Some(pkm_event::Event::RunAgentTurn(_))
        ));

        let err = PkmEvent {
            event: Some(pkm_event::Event::ResolveError(ResolveError {
                code: 1,
                message: "fail".into(),
            })),
        };
        assert!(matches!(err.event, Some(pkm_event::Event::ResolveError(_))));
    }
}
