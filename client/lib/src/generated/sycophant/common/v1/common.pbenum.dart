// This is a generated file - do not edit.
//
// Generated from sycophant/common/v1/common.proto.

// @dart = 3.3

// ignore_for_file: annotate_overrides, camel_case_types, comment_references
// ignore_for_file: constant_identifier_names
// ignore_for_file: curly_braces_in_flow_control_structures
// ignore_for_file: deprecated_member_use_from_same_package, library_prefixes
// ignore_for_file: non_constant_identifier_names, prefer_relative_imports

import 'dart:core' as $core;

import 'package:protobuf/protobuf.dart' as $pb;

class StopReason extends $pb.ProtobufEnum {
  static const StopReason STOP_REASON_UNSPECIFIED =
      StopReason._(0, _omitEnumNames ? '' : 'STOP_REASON_UNSPECIFIED');
  static const StopReason END_TURN =
      StopReason._(1, _omitEnumNames ? '' : 'END_TURN');
  static const StopReason TOOL_USE =
      StopReason._(2, _omitEnumNames ? '' : 'TOOL_USE');
  static const StopReason MAX_TOKENS =
      StopReason._(3, _omitEnumNames ? '' : 'MAX_TOKENS');

  static const $core.List<StopReason> values = <StopReason>[
    STOP_REASON_UNSPECIFIED,
    END_TURN,
    TOOL_USE,
    MAX_TOKENS,
  ];

  static final $core.List<StopReason?> _byValue =
      $pb.ProtobufEnum.$_initByValueList(values, 3);
  static StopReason? valueOf($core.int value) =>
      value < 0 || value >= _byValue.length ? null : _byValue[value];

  const StopReason._(super.value, super.name);
}

class TurnState extends $pb.ProtobufEnum {
  static const TurnState TURN_STATE_UNSPECIFIED =
      TurnState._(0, _omitEnumNames ? '' : 'TURN_STATE_UNSPECIFIED');

  /// No active turn. Default for fresh channels; emitted after the
  /// assistant message persists.
  static const TurnState TURN_STATE_IDLE =
      TurnState._(1, _omitEnumNames ? '' : 'TURN_STATE_IDLE');

  /// Turn is in flight on the cluster: controller enqueued the user
  /// message, transponder loop is running.
  static const TurnState TURN_STATE_WORKING =
      TurnState._(2, _omitEnumNames ? '' : 'TURN_STATE_WORKING');

  /// Reserved (comment-only, not proto `reserved` since these slots are
  /// free):
  ///   3 — THINKING; derived from content-delta forwarding.
  ///   4 — STOPPING; paired with cancel/interrupt RPC.
  /// Turn ended in failure (worker reaped/crashed, idle-timeout, or a
  /// TurnError). Carries a reason/code on TurnStateEvent. Distinct from
  /// IDLE so the client can show an actionable error and re-enable input.
  static const TurnState TURN_STATE_FAILED =
      TurnState._(5, _omitEnumNames ? '' : 'TURN_STATE_FAILED');

  /// Turn was cancelled by the client (local stop). Terminal, like FAILED,
  /// but not an error — client re-enables input without an error banner.
  static const TurnState TURN_STATE_CANCELLED =
      TurnState._(6, _omitEnumNames ? '' : 'TURN_STATE_CANCELLED');

  static const $core.List<TurnState> values = <TurnState>[
    TURN_STATE_UNSPECIFIED,
    TURN_STATE_IDLE,
    TURN_STATE_WORKING,
    TURN_STATE_FAILED,
    TURN_STATE_CANCELLED,
  ];

  static final $core.List<TurnState?> _byValue =
      $pb.ProtobufEnum.$_initByValueList(values, 6);
  static TurnState? valueOf($core.int value) =>
      value < 0 || value >= _byValue.length ? null : _byValue[value];

  const TurnState._(super.value, super.name);
}

/// How a tool call ended. The runtime is the only party that can tell a
/// user cancel from a timeout from a genuine non-zero exit, so it stamps one
/// of these on the terminal frame. Every downstream error state is derived as
/// `outcome != DONE`, so error and outcome can never contradict. Mirrors
/// TurnState's terminal split (FAILED / CANCELLED), with one L on CANCELED.
class ToolOutcome extends $pb.ProtobufEnum {
  static const ToolOutcome TOOL_OUTCOME_UNSPECIFIED =
      ToolOutcome._(0, _omitEnumNames ? '' : 'TOOL_OUTCOME_UNSPECIFIED');

  /// Clean exit (exit code 0). The only non-error outcome.
  static const ToolOutcome TOOL_OUTCOME_DONE =
      ToolOutcome._(1, _omitEnumNames ? '' : 'TOOL_OUTCOME_DONE');

  /// A genuine non-zero exit, an image-reference failure, or a timeout —
  /// every non-cancel failure folds here.
  static const ToolOutcome TOOL_OUTCOME_FAILED =
      ToolOutcome._(2, _omitEnumNames ? '' : 'TOOL_OUTCOME_FAILED');

  /// A user cancel killed the child. Terminal but not an error: the killed
  /// child still reports the -1 sentinel exit, only the outcome distinguishes it.
  static const ToolOutcome TOOL_OUTCOME_CANCELED =
      ToolOutcome._(3, _omitEnumNames ? '' : 'TOOL_OUTCOME_CANCELED');

  static const $core.List<ToolOutcome> values = <ToolOutcome>[
    TOOL_OUTCOME_UNSPECIFIED,
    TOOL_OUTCOME_DONE,
    TOOL_OUTCOME_FAILED,
    TOOL_OUTCOME_CANCELED,
  ];

  static final $core.List<ToolOutcome?> _byValue =
      $pb.ProtobufEnum.$_initByValueList(values, 3);
  static ToolOutcome? valueOf($core.int value) =>
      value < 0 || value >= _byValue.length ? null : _byValue[value];

  const ToolOutcome._(super.value, super.name);
}

const $core.bool _omitEnumNames =
    $core.bool.fromEnvironment('protobuf.omit_enum_names');
