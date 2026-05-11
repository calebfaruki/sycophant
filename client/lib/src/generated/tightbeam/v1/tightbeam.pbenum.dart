// This is a generated file - do not edit.
//
// Generated from tightbeam/v1/tightbeam.proto.

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

class TurnRole extends $pb.ProtobufEnum {
  static const TurnRole TURN_ROLE_UNSPECIFIED =
      TurnRole._(0, _omitEnumNames ? '' : 'TURN_ROLE_UNSPECIFIED');
  static const TurnRole DELEGATE =
      TurnRole._(3, _omitEnumNames ? '' : 'DELEGATE');

  static const $core.List<TurnRole> values = <TurnRole>[
    TURN_ROLE_UNSPECIFIED,
    DELEGATE,
  ];

  static final $core.Map<$core.int, TurnRole> _byValue =
      $pb.ProtobufEnum.initByValue(values);
  static TurnRole? valueOf($core.int value) => _byValue[value];

  const TurnRole._(super.value, super.name);
}

const $core.bool _omitEnumNames =
    $core.bool.fromEnvironment('protobuf.omit_enum_names');
