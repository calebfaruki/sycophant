//! The inference-job result frame is `TurnEvent` — the same event stream the
//! harness turn loop already consumes. The harness dials the pod and owns the
//! connection, so the frame must carry nothing that names a next hop: no
//! return-audience, who-is-next, or handoff field. A dispatch that decided who
//! runs next would reintroduce the controller-brokered turn hand-off this
//! design removes.
//!
//! This is a compile-time guard: the match over every event variant is
//! exhaustive and names each variant explicitly, so adding a handoff variant
//! (or renaming one to carry a next-hop) fails to compile. The command oneof is
//! guarded the same way.

use toolset_proto::{
    run_inference_command, turn_event, ContentDelta, RunInferenceCommand, TurnAssignment,
    TurnComplete, TurnError, TurnEvent, TurnWarning,
};

/// Every result-frame variant, named exhaustively. If a variant is added or
/// removed this stops compiling, which is the assertion: the frame's shape is
/// pinned to content/control events with no next-hop field.
fn classify_frame(frame: &TurnEvent) -> &'static str {
    match frame.event.as_ref() {
        Some(turn_event::Event::ContentDelta(_)) => "content_delta",
        Some(turn_event::Event::ToolUseStart(_)) => "tool_use_start",
        Some(turn_event::Event::ToolUseInput(_)) => "tool_use_input",
        Some(turn_event::Event::Complete(_)) => "complete",
        Some(turn_event::Event::Error(_)) => "error",
        Some(turn_event::Event::Warning(_)) => "warning",
        None => "empty",
    }
}

#[test]
fn the_inference_result_frame_carries_only_content_and_control_events() {
    let cases = [
        (
            TurnEvent {
                event: Some(turn_event::Event::ContentDelta(ContentDelta {
                    text: "hi".into(),
                })),
            },
            "content_delta",
        ),
        (
            TurnEvent {
                event: Some(turn_event::Event::Complete(TurnComplete {
                    stop_reason: 0,
                    content: vec![],
                    tool_calls: vec![],
                })),
            },
            "complete",
        ),
        (
            TurnEvent {
                event: Some(turn_event::Event::Error(TurnError {
                    code: 1,
                    message: "e".into(),
                })),
            },
            "error",
        ),
        (
            TurnEvent {
                event: Some(turn_event::Event::Warning(TurnWarning {
                    field: "f".into(),
                    reason: "r".into(),
                })),
            },
            "warning",
        ),
    ];
    for (frame, want) in cases {
        assert_eq!(classify_frame(&frame), want);
    }
}

/// The command oneof carries exactly the assignment and cancel arms, mirroring
/// ToolJob. No third arm can smuggle a return-audience onto the outbound half.
#[test]
fn the_inference_command_carries_only_assignment_and_cancel() {
    fn classify_command(cmd: &RunInferenceCommand) -> &'static str {
        match cmd.command.as_ref() {
            Some(run_inference_command::Command::Assignment(_)) => "assignment",
            Some(run_inference_command::Command::Cancel(_)) => "cancel",
            None => "empty",
        }
    }
    let assignment = RunInferenceCommand {
        command: Some(run_inference_command::Command::Assignment(TurnAssignment {
            system: None,
            tools: vec![],
            messages: vec![],
            conversation_id: "c".into(),
        })),
    };
    assert_eq!(classify_command(&assignment), "assignment");
}
