// This is a generated file - do not edit.
//
// Generated from tightbeam/v1/tightbeam.proto.

// @dart = 3.3

// ignore_for_file: annotate_overrides, camel_case_types, comment_references
// ignore_for_file: constant_identifier_names
// ignore_for_file: curly_braces_in_flow_control_structures
// ignore_for_file: deprecated_member_use_from_same_package, library_prefixes
// ignore_for_file: non_constant_identifier_names, prefer_relative_imports
// ignore_for_file: unused_import

import 'dart:convert' as $convert;
import 'dart:core' as $core;
import 'dart:typed_data' as $typed_data;

@$core.Deprecated('Use stopReasonDescriptor instead')
const StopReason$json = {
  '1': 'StopReason',
  '2': [
    {'1': 'STOP_REASON_UNSPECIFIED', '2': 0},
    {'1': 'END_TURN', '2': 1},
    {'1': 'TOOL_USE', '2': 2},
    {'1': 'MAX_TOKENS', '2': 3},
  ],
};

/// Descriptor for `StopReason`. Decode as a `google.protobuf.EnumDescriptorProto`.
final $typed_data.Uint8List stopReasonDescriptor = $convert.base64Decode(
    'CgpTdG9wUmVhc29uEhsKF1NUT1BfUkVBU09OX1VOU1BFQ0lGSUVEEAASDAoIRU5EX1RVUk4QAR'
    'IMCghUT09MX1VTRRACEg4KCk1BWF9UT0tFTlMQAw==');

@$core.Deprecated('Use turnRoleDescriptor instead')
const TurnRole$json = {
  '1': 'TurnRole',
  '2': [
    {'1': 'TURN_ROLE_UNSPECIFIED', '2': 0},
    {'1': 'DELEGATE', '2': 3},
  ],
};

/// Descriptor for `TurnRole`. Decode as a `google.protobuf.EnumDescriptorProto`.
final $typed_data.Uint8List turnRoleDescriptor = $convert.base64Decode(
    'CghUdXJuUm9sZRIZChVUVVJOX1JPTEVfVU5TUEVDSUZJRUQQABIMCghERUxFR0FURRAD');

@$core.Deprecated('Use contentBlockDescriptor instead')
const ContentBlock$json = {
  '1': 'ContentBlock',
  '2': [
    {
      '1': 'text',
      '3': 1,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.TextBlock',
      '9': 0,
      '10': 'text'
    },
    {
      '1': 'image',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ImageBlock',
      '9': 0,
      '10': 'image'
    },
    {
      '1': 'thinking',
      '3': 3,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ThinkingBlock',
      '9': 0,
      '10': 'thinking'
    },
  ],
  '8': [
    {'1': 'block'},
  ],
};

/// Descriptor for `ContentBlock`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List contentBlockDescriptor = $convert.base64Decode(
    'CgxDb250ZW50QmxvY2sSLQoEdGV4dBgBIAEoCzIXLnRpZ2h0YmVhbS52MS5UZXh0QmxvY2tIAF'
    'IEdGV4dBIwCgVpbWFnZRgCIAEoCzIYLnRpZ2h0YmVhbS52MS5JbWFnZUJsb2NrSABSBWltYWdl'
    'EjkKCHRoaW5raW5nGAMgASgLMhsudGlnaHRiZWFtLnYxLlRoaW5raW5nQmxvY2tIAFIIdGhpbm'
    'tpbmdCBwoFYmxvY2s=');

@$core.Deprecated('Use textBlockDescriptor instead')
const TextBlock$json = {
  '1': 'TextBlock',
  '2': [
    {'1': 'text', '3': 1, '4': 1, '5': 9, '10': 'text'},
  ],
};

/// Descriptor for `TextBlock`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List textBlockDescriptor =
    $convert.base64Decode('CglUZXh0QmxvY2sSEgoEdGV4dBgBIAEoCVIEdGV4dA==');

@$core.Deprecated('Use imageBlockDescriptor instead')
const ImageBlock$json = {
  '1': 'ImageBlock',
  '2': [
    {'1': 'media_type', '3': 1, '4': 1, '5': 9, '10': 'mediaType'},
    {'1': 'data', '3': 2, '4': 1, '5': 12, '10': 'data'},
  ],
};

/// Descriptor for `ImageBlock`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List imageBlockDescriptor = $convert.base64Decode(
    'CgpJbWFnZUJsb2NrEh0KCm1lZGlhX3R5cGUYASABKAlSCW1lZGlhVHlwZRISCgRkYXRhGAIgAS'
    'gMUgRkYXRh');

@$core.Deprecated('Use thinkingBlockDescriptor instead')
const ThinkingBlock$json = {
  '1': 'ThinkingBlock',
  '2': [
    {'1': 'text', '3': 1, '4': 1, '5': 9, '10': 'text'},
  ],
};

/// Descriptor for `ThinkingBlock`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List thinkingBlockDescriptor =
    $convert.base64Decode('Cg1UaGlua2luZ0Jsb2NrEhIKBHRleHQYASABKAlSBHRleHQ=');

@$core.Deprecated('Use toolDefinitionDescriptor instead')
const ToolDefinition$json = {
  '1': 'ToolDefinition',
  '2': [
    {'1': 'name', '3': 1, '4': 1, '5': 9, '10': 'name'},
    {'1': 'description', '3': 2, '4': 1, '5': 9, '10': 'description'},
    {'1': 'parameters_json', '3': 3, '4': 1, '5': 9, '10': 'parametersJson'},
  ],
};

/// Descriptor for `ToolDefinition`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List toolDefinitionDescriptor = $convert.base64Decode(
    'Cg5Ub29sRGVmaW5pdGlvbhISCgRuYW1lGAEgASgJUgRuYW1lEiAKC2Rlc2NyaXB0aW9uGAIgAS'
    'gJUgtkZXNjcmlwdGlvbhInCg9wYXJhbWV0ZXJzX2pzb24YAyABKAlSDnBhcmFtZXRlcnNKc29u');

@$core.Deprecated('Use toolCallDescriptor instead')
const ToolCall$json = {
  '1': 'ToolCall',
  '2': [
    {'1': 'id', '3': 1, '4': 1, '5': 9, '10': 'id'},
    {'1': 'name', '3': 2, '4': 1, '5': 9, '10': 'name'},
    {'1': 'input_json', '3': 3, '4': 1, '5': 9, '10': 'inputJson'},
  ],
};

/// Descriptor for `ToolCall`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List toolCallDescriptor = $convert.base64Decode(
    'CghUb29sQ2FsbBIOCgJpZBgBIAEoCVICaWQSEgoEbmFtZRgCIAEoCVIEbmFtZRIdCgppbnB1dF'
    '9qc29uGAMgASgJUglpbnB1dEpzb24=');

@$core.Deprecated('Use messageDescriptor instead')
const Message$json = {
  '1': 'Message',
  '2': [
    {'1': 'role', '3': 1, '4': 1, '5': 9, '10': 'role'},
    {
      '1': 'content',
      '3': 2,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.ContentBlock',
      '10': 'content'
    },
    {
      '1': 'tool_calls',
      '3': 3,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.ToolCall',
      '10': 'toolCalls'
    },
    {
      '1': 'tool_call_id',
      '3': 4,
      '4': 1,
      '5': 9,
      '9': 0,
      '10': 'toolCallId',
      '17': true
    },
    {
      '1': 'is_error',
      '3': 5,
      '4': 1,
      '5': 8,
      '9': 1,
      '10': 'isError',
      '17': true
    },
  ],
  '8': [
    {'1': '_tool_call_id'},
    {'1': '_is_error'},
  ],
};

/// Descriptor for `Message`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List messageDescriptor = $convert.base64Decode(
    'CgdNZXNzYWdlEhIKBHJvbGUYASABKAlSBHJvbGUSNAoHY29udGVudBgCIAMoCzIaLnRpZ2h0Ym'
    'VhbS52MS5Db250ZW50QmxvY2tSB2NvbnRlbnQSNQoKdG9vbF9jYWxscxgDIAMoCzIWLnRpZ2h0'
    'YmVhbS52MS5Ub29sQ2FsbFIJdG9vbENhbGxzEiUKDHRvb2xfY2FsbF9pZBgEIAEoCUgAUgp0b2'
    '9sQ2FsbElkiAEBEh4KCGlzX2Vycm9yGAUgASgISAFSB2lzRXJyb3KIAQFCDwoNX3Rvb2xfY2Fs'
    'bF9pZEILCglfaXNfZXJyb3I=');

@$core.Deprecated('Use getTurnRequestDescriptor instead')
const GetTurnRequest$json = {
  '1': 'GetTurnRequest',
  '2': [
    {'1': 'model_name', '3': 1, '4': 1, '5': 9, '10': 'modelName'},
  ],
};

/// Descriptor for `GetTurnRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List getTurnRequestDescriptor = $convert.base64Decode(
    'Cg5HZXRUdXJuUmVxdWVzdBIdCgptb2RlbF9uYW1lGAEgASgJUgltb2RlbE5hbWU=');

@$core.Deprecated('Use turnAssignmentDescriptor instead')
const TurnAssignment$json = {
  '1': 'TurnAssignment',
  '2': [
    {'1': 'system', '3': 1, '4': 1, '5': 9, '9': 0, '10': 'system', '17': true},
    {
      '1': 'tools',
      '3': 2,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.ToolDefinition',
      '10': 'tools'
    },
    {
      '1': 'messages',
      '3': 3,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.Message',
      '10': 'messages'
    },
    {
      '1': 'params_json',
      '3': 4,
      '4': 1,
      '5': 9,
      '9': 1,
      '10': 'paramsJson',
      '17': true
    },
  ],
  '8': [
    {'1': '_system'},
    {'1': '_params_json'},
  ],
};

/// Descriptor for `TurnAssignment`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List turnAssignmentDescriptor = $convert.base64Decode(
    'Cg5UdXJuQXNzaWdubWVudBIbCgZzeXN0ZW0YASABKAlIAFIGc3lzdGVtiAEBEjIKBXRvb2xzGA'
    'IgAygLMhwudGlnaHRiZWFtLnYxLlRvb2xEZWZpbml0aW9uUgV0b29scxIxCghtZXNzYWdlcxgD'
    'IAMoCzIVLnRpZ2h0YmVhbS52MS5NZXNzYWdlUghtZXNzYWdlcxIkCgtwYXJhbXNfanNvbhgEIA'
    'EoCUgBUgpwYXJhbXNKc29uiAEBQgkKB19zeXN0ZW1CDgoMX3BhcmFtc19qc29u');

@$core.Deprecated('Use turnResultChunkDescriptor instead')
const TurnResultChunk$json = {
  '1': 'TurnResultChunk',
  '2': [
    {
      '1': 'content_delta',
      '3': 1,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ContentDelta',
      '9': 0,
      '10': 'contentDelta'
    },
    {
      '1': 'tool_use_start',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ToolUseStart',
      '9': 0,
      '10': 'toolUseStart'
    },
    {
      '1': 'tool_use_input',
      '3': 3,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ToolUseInput',
      '9': 0,
      '10': 'toolUseInput'
    },
    {
      '1': 'complete',
      '3': 4,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.TurnComplete',
      '9': 0,
      '10': 'complete'
    },
    {
      '1': 'error',
      '3': 5,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.TurnError',
      '9': 0,
      '10': 'error'
    },
    {
      '1': 'warning',
      '3': 6,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.TurnWarning',
      '9': 0,
      '10': 'warning'
    },
  ],
  '8': [
    {'1': 'chunk'},
  ],
};

/// Descriptor for `TurnResultChunk`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List turnResultChunkDescriptor = $convert.base64Decode(
    'Cg9UdXJuUmVzdWx0Q2h1bmsSQQoNY29udGVudF9kZWx0YRgBIAEoCzIaLnRpZ2h0YmVhbS52MS'
    '5Db250ZW50RGVsdGFIAFIMY29udGVudERlbHRhEkIKDnRvb2xfdXNlX3N0YXJ0GAIgASgLMhou'
    'dGlnaHRiZWFtLnYxLlRvb2xVc2VTdGFydEgAUgx0b29sVXNlU3RhcnQSQgoOdG9vbF91c2VfaW'
    '5wdXQYAyABKAsyGi50aWdodGJlYW0udjEuVG9vbFVzZUlucHV0SABSDHRvb2xVc2VJbnB1dBI4'
    'Cghjb21wbGV0ZRgEIAEoCzIaLnRpZ2h0YmVhbS52MS5UdXJuQ29tcGxldGVIAFIIY29tcGxldG'
    'USLwoFZXJyb3IYBSABKAsyFy50aWdodGJlYW0udjEuVHVybkVycm9ySABSBWVycm9yEjUKB3dh'
    'cm5pbmcYBiABKAsyGS50aWdodGJlYW0udjEuVHVybldhcm5pbmdIAFIHd2FybmluZ0IHCgVjaH'
    'Vuaw==');

@$core.Deprecated('Use contentDeltaDescriptor instead')
const ContentDelta$json = {
  '1': 'ContentDelta',
  '2': [
    {'1': 'text', '3': 1, '4': 1, '5': 9, '10': 'text'},
  ],
};

/// Descriptor for `ContentDelta`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List contentDeltaDescriptor =
    $convert.base64Decode('CgxDb250ZW50RGVsdGESEgoEdGV4dBgBIAEoCVIEdGV4dA==');

@$core.Deprecated('Use toolUseStartDescriptor instead')
const ToolUseStart$json = {
  '1': 'ToolUseStart',
  '2': [
    {'1': 'id', '3': 1, '4': 1, '5': 9, '10': 'id'},
    {'1': 'name', '3': 2, '4': 1, '5': 9, '10': 'name'},
  ],
};

/// Descriptor for `ToolUseStart`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List toolUseStartDescriptor = $convert.base64Decode(
    'CgxUb29sVXNlU3RhcnQSDgoCaWQYASABKAlSAmlkEhIKBG5hbWUYAiABKAlSBG5hbWU=');

@$core.Deprecated('Use toolUseInputDescriptor instead')
const ToolUseInput$json = {
  '1': 'ToolUseInput',
  '2': [
    {'1': 'partial_json', '3': 1, '4': 1, '5': 9, '10': 'partialJson'},
  ],
};

/// Descriptor for `ToolUseInput`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List toolUseInputDescriptor = $convert.base64Decode(
    'CgxUb29sVXNlSW5wdXQSIQoMcGFydGlhbF9qc29uGAEgASgJUgtwYXJ0aWFsSnNvbg==');

@$core.Deprecated('Use turnCompleteDescriptor instead')
const TurnComplete$json = {
  '1': 'TurnComplete',
  '2': [
    {
      '1': 'stop_reason',
      '3': 1,
      '4': 1,
      '5': 14,
      '6': '.tightbeam.v1.StopReason',
      '10': 'stopReason'
    },
    {
      '1': 'content',
      '3': 2,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.ContentBlock',
      '10': 'content'
    },
    {
      '1': 'tool_calls',
      '3': 3,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.ToolCall',
      '10': 'toolCalls'
    },
  ],
};

/// Descriptor for `TurnComplete`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List turnCompleteDescriptor = $convert.base64Decode(
    'CgxUdXJuQ29tcGxldGUSOQoLc3RvcF9yZWFzb24YASABKA4yGC50aWdodGJlYW0udjEuU3RvcF'
    'JlYXNvblIKc3RvcFJlYXNvbhI0Cgdjb250ZW50GAIgAygLMhoudGlnaHRiZWFtLnYxLkNvbnRl'
    'bnRCbG9ja1IHY29udGVudBI1Cgp0b29sX2NhbGxzGAMgAygLMhYudGlnaHRiZWFtLnYxLlRvb2'
    'xDYWxsUgl0b29sQ2FsbHM=');

@$core.Deprecated('Use turnErrorDescriptor instead')
const TurnError$json = {
  '1': 'TurnError',
  '2': [
    {'1': 'code', '3': 1, '4': 1, '5': 5, '10': 'code'},
    {'1': 'message', '3': 2, '4': 1, '5': 9, '10': 'message'},
  ],
};

/// Descriptor for `TurnError`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List turnErrorDescriptor = $convert.base64Decode(
    'CglUdXJuRXJyb3ISEgoEY29kZRgBIAEoBVIEY29kZRIYCgdtZXNzYWdlGAIgASgJUgdtZXNzYW'
    'dl');

@$core.Deprecated('Use turnWarningDescriptor instead')
const TurnWarning$json = {
  '1': 'TurnWarning',
  '2': [
    {'1': 'field', '3': 1, '4': 1, '5': 9, '10': 'field'},
    {'1': 'reason', '3': 2, '4': 1, '5': 9, '10': 'reason'},
  ],
};

/// Descriptor for `TurnWarning`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List turnWarningDescriptor = $convert.base64Decode(
    'CgtUdXJuV2FybmluZxIUCgVmaWVsZBgBIAEoCVIFZmllbGQSFgoGcmVhc29uGAIgASgJUgZyZW'
    'Fzb24=');

@$core.Deprecated('Use turnAckDescriptor instead')
const TurnAck$json = {
  '1': 'TurnAck',
};

/// Descriptor for `TurnAck`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List turnAckDescriptor =
    $convert.base64Decode('CgdUdXJuQWNr');

@$core.Deprecated('Use turnRequestDescriptor instead')
const TurnRequest$json = {
  '1': 'TurnRequest',
  '2': [
    {'1': 'system', '3': 1, '4': 1, '5': 9, '9': 0, '10': 'system', '17': true},
    {
      '1': 'tools',
      '3': 2,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.ToolDefinition',
      '10': 'tools'
    },
    {
      '1': 'messages',
      '3': 3,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.Message',
      '10': 'messages'
    },
    {'1': 'model', '3': 5, '4': 1, '5': 9, '9': 1, '10': 'model', '17': true},
    {
      '1': 'reply_channel',
      '3': 6,
      '4': 1,
      '5': 9,
      '9': 2,
      '10': 'replyChannel',
      '17': true
    },
    {
      '1': 'role',
      '3': 7,
      '4': 1,
      '5': 14,
      '6': '.tightbeam.v1.TurnRole',
      '9': 3,
      '10': 'role',
      '17': true
    },
    {
      '1': 'correlation_id',
      '3': 9,
      '4': 1,
      '5': 9,
      '9': 4,
      '10': 'correlationId',
      '17': true
    },
    {'1': 'conversation_id', '3': 10, '4': 1, '5': 9, '10': 'conversationId'},
  ],
  '8': [
    {'1': '_system'},
    {'1': '_model'},
    {'1': '_reply_channel'},
    {'1': '_role'},
    {'1': '_correlation_id'},
  ],
};

/// Descriptor for `TurnRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List turnRequestDescriptor = $convert.base64Decode(
    'CgtUdXJuUmVxdWVzdBIbCgZzeXN0ZW0YASABKAlIAFIGc3lzdGVtiAEBEjIKBXRvb2xzGAIgAy'
    'gLMhwudGlnaHRiZWFtLnYxLlRvb2xEZWZpbml0aW9uUgV0b29scxIxCghtZXNzYWdlcxgDIAMo'
    'CzIVLnRpZ2h0YmVhbS52MS5NZXNzYWdlUghtZXNzYWdlcxIZCgVtb2RlbBgFIAEoCUgBUgVtb2'
    'RlbIgBARIoCg1yZXBseV9jaGFubmVsGAYgASgJSAJSDHJlcGx5Q2hhbm5lbIgBARIvCgRyb2xl'
    'GAcgASgOMhYudGlnaHRiZWFtLnYxLlR1cm5Sb2xlSANSBHJvbGWIAQESKgoOY29ycmVsYXRpb2'
    '5faWQYCSABKAlIBFINY29ycmVsYXRpb25JZIgBARInCg9jb252ZXJzYXRpb25faWQYCiABKAlS'
    'DmNvbnZlcnNhdGlvbklkQgkKB19zeXN0ZW1CCAoGX21vZGVsQhAKDl9yZXBseV9jaGFubmVsQg'
    'cKBV9yb2xlQhEKD19jb3JyZWxhdGlvbl9pZA==');

@$core.Deprecated('Use turnEventDescriptor instead')
const TurnEvent$json = {
  '1': 'TurnEvent',
  '2': [
    {
      '1': 'content_delta',
      '3': 1,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ContentDelta',
      '9': 0,
      '10': 'contentDelta'
    },
    {
      '1': 'tool_use_start',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ToolUseStart',
      '9': 0,
      '10': 'toolUseStart'
    },
    {
      '1': 'tool_use_input',
      '3': 3,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ToolUseInput',
      '9': 0,
      '10': 'toolUseInput'
    },
    {
      '1': 'complete',
      '3': 4,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.TurnComplete',
      '9': 0,
      '10': 'complete'
    },
    {
      '1': 'error',
      '3': 5,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.TurnError',
      '9': 0,
      '10': 'error'
    },
    {
      '1': 'warning',
      '3': 6,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.TurnWarning',
      '9': 0,
      '10': 'warning'
    },
  ],
  '8': [
    {'1': 'event'},
  ],
};

/// Descriptor for `TurnEvent`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List turnEventDescriptor = $convert.base64Decode(
    'CglUdXJuRXZlbnQSQQoNY29udGVudF9kZWx0YRgBIAEoCzIaLnRpZ2h0YmVhbS52MS5Db250ZW'
    '50RGVsdGFIAFIMY29udGVudERlbHRhEkIKDnRvb2xfdXNlX3N0YXJ0GAIgASgLMhoudGlnaHRi'
    'ZWFtLnYxLlRvb2xVc2VTdGFydEgAUgx0b29sVXNlU3RhcnQSQgoOdG9vbF91c2VfaW5wdXQYAy'
    'ABKAsyGi50aWdodGJlYW0udjEuVG9vbFVzZUlucHV0SABSDHRvb2xVc2VJbnB1dBI4Cghjb21w'
    'bGV0ZRgEIAEoCzIaLnRpZ2h0YmVhbS52MS5UdXJuQ29tcGxldGVIAFIIY29tcGxldGUSLwoFZX'
    'Jyb3IYBSABKAsyFy50aWdodGJlYW0udjEuVHVybkVycm9ySABSBWVycm9yEjUKB3dhcm5pbmcY'
    'BiABKAsyGS50aWdodGJlYW0udjEuVHVybldhcm5pbmdIAFIHd2FybmluZ0IHCgVldmVudA==');

@$core.Deprecated('Use mintConversationRequestDescriptor instead')
const MintConversationRequest$json = {
  '1': 'MintConversationRequest',
};

/// Descriptor for `MintConversationRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List mintConversationRequestDescriptor =
    $convert.base64Decode('ChdNaW50Q29udmVyc2F0aW9uUmVxdWVzdA==');

@$core.Deprecated('Use mintConversationResponseDescriptor instead')
const MintConversationResponse$json = {
  '1': 'MintConversationResponse',
  '2': [
    {'1': 'conversation_id', '3': 1, '4': 1, '5': 9, '10': 'conversationId'},
  ],
};

/// Descriptor for `MintConversationResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List mintConversationResponseDescriptor =
    $convert.base64Decode(
        'ChhNaW50Q29udmVyc2F0aW9uUmVzcG9uc2USJwoPY29udmVyc2F0aW9uX2lkGAEgASgJUg5jb2'
        '52ZXJzYXRpb25JZA==');

@$core.Deprecated('Use listConversationsRequestDescriptor instead')
const ListConversationsRequest$json = {
  '1': 'ListConversationsRequest',
  '2': [
    {'1': 'workspace', '3': 1, '4': 1, '5': 9, '10': 'workspace'},
  ],
};

/// Descriptor for `ListConversationsRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List listConversationsRequestDescriptor =
    $convert.base64Decode(
        'ChhMaXN0Q29udmVyc2F0aW9uc1JlcXVlc3QSHAoJd29ya3NwYWNlGAEgASgJUgl3b3Jrc3BhY2'
        'U=');

@$core.Deprecated('Use listConversationsResponseDescriptor instead')
const ListConversationsResponse$json = {
  '1': 'ListConversationsResponse',
  '2': [
    {'1': 'conversation_ids', '3': 1, '4': 3, '5': 9, '10': 'conversationIds'},
  ],
};

/// Descriptor for `ListConversationsResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List listConversationsResponseDescriptor =
    $convert.base64Decode(
        'ChlMaXN0Q29udmVyc2F0aW9uc1Jlc3BvbnNlEikKEGNvbnZlcnNhdGlvbl9pZHMYASADKAlSD2'
        'NvbnZlcnNhdGlvbklkcw==');

@$core.Deprecated('Use listWorkspacesRequestDescriptor instead')
const ListWorkspacesRequest$json = {
  '1': 'ListWorkspacesRequest',
};

/// Descriptor for `ListWorkspacesRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List listWorkspacesRequestDescriptor =
    $convert.base64Decode('ChVMaXN0V29ya3NwYWNlc1JlcXVlc3Q=');

@$core.Deprecated('Use listWorkspacesResponseDescriptor instead')
const ListWorkspacesResponse$json = {
  '1': 'ListWorkspacesResponse',
  '2': [
    {'1': 'workspaces', '3': 1, '4': 3, '5': 9, '10': 'workspaces'},
  ],
};

/// Descriptor for `ListWorkspacesResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List listWorkspacesResponseDescriptor =
    $convert.base64Decode(
        'ChZMaXN0V29ya3NwYWNlc1Jlc3BvbnNlEh4KCndvcmtzcGFjZXMYASADKAlSCndvcmtzcGFjZX'
        'M=');

@$core.Deprecated('Use channelInboundDescriptor instead')
const ChannelInbound$json = {
  '1': 'ChannelInbound',
  '2': [
    {
      '1': 'register',
      '3': 1,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ChannelRegister',
      '9': 0,
      '10': 'register'
    },
    {
      '1': 'user_message',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.UserMessage',
      '9': 0,
      '10': 'userMessage'
    },
  ],
  '8': [
    {'1': 'event'},
  ],
};

/// Descriptor for `ChannelInbound`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelInboundDescriptor = $convert.base64Decode(
    'Cg5DaGFubmVsSW5ib3VuZBI7CghyZWdpc3RlchgBIAEoCzIdLnRpZ2h0YmVhbS52MS5DaGFubm'
    'VsUmVnaXN0ZXJIAFIIcmVnaXN0ZXISPgoMdXNlcl9tZXNzYWdlGAIgASgLMhkudGlnaHRiZWFt'
    'LnYxLlVzZXJNZXNzYWdlSABSC3VzZXJNZXNzYWdlQgcKBWV2ZW50');

@$core.Deprecated('Use channelRegisterDescriptor instead')
const ChannelRegister$json = {
  '1': 'ChannelRegister',
  '2': [
    {
      '1': 'workspace',
      '3': 1,
      '4': 1,
      '5': 9,
      '9': 0,
      '10': 'workspace',
      '17': true
    },
    {
      '1': 'adapter_hint',
      '3': 2,
      '4': 1,
      '5': 9,
      '9': 1,
      '10': 'adapterHint',
      '17': true
    },
  ],
  '8': [
    {'1': '_workspace'},
    {'1': '_adapter_hint'},
  ],
};

/// Descriptor for `ChannelRegister`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelRegisterDescriptor = $convert.base64Decode(
    'Cg9DaGFubmVsUmVnaXN0ZXISIQoJd29ya3NwYWNlGAEgASgJSABSCXdvcmtzcGFjZYgBARImCg'
    'xhZGFwdGVyX2hpbnQYAiABKAlIAVILYWRhcHRlckhpbnSIAQFCDAoKX3dvcmtzcGFjZUIPCg1f'
    'YWRhcHRlcl9oaW50');

@$core.Deprecated('Use channelAckDescriptor instead')
const ChannelAck$json = {
  '1': 'ChannelAck',
  '2': [
    {'1': 'channel_id', '3': 1, '4': 1, '5': 9, '10': 'channelId'},
  ],
};

/// Descriptor for `ChannelAck`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelAckDescriptor = $convert.base64Decode(
    'CgpDaGFubmVsQWNrEh0KCmNoYW5uZWxfaWQYASABKAlSCWNoYW5uZWxJZA==');

@$core.Deprecated('Use channelOutboundDescriptor instead')
const ChannelOutbound$json = {
  '1': 'ChannelOutbound',
  '2': [
    {
      '1': 'ack',
      '3': 1,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ChannelAck',
      '9': 0,
      '10': 'ack'
    },
    {
      '1': 'send_message',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ChannelSend',
      '9': 0,
      '10': 'sendMessage'
    },
  ],
  '8': [
    {'1': 'command'},
  ],
};

/// Descriptor for `ChannelOutbound`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelOutboundDescriptor = $convert.base64Decode(
    'Cg9DaGFubmVsT3V0Ym91bmQSLAoDYWNrGAEgASgLMhgudGlnaHRiZWFtLnYxLkNoYW5uZWxBY2'
    'tIAFIDYWNrEj4KDHNlbmRfbWVzc2FnZRgCIAEoCzIZLnRpZ2h0YmVhbS52MS5DaGFubmVsU2Vu'
    'ZEgAUgtzZW5kTWVzc2FnZUIJCgdjb21tYW5k');

@$core.Deprecated('Use channelSendDescriptor instead')
const ChannelSend$json = {
  '1': 'ChannelSend',
  '2': [
    {
      '1': 'content',
      '3': 1,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.ContentBlock',
      '10': 'content'
    },
  ],
};

/// Descriptor for `ChannelSend`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelSendDescriptor = $convert.base64Decode(
    'CgtDaGFubmVsU2VuZBI0Cgdjb250ZW50GAEgAygLMhoudGlnaHRiZWFtLnYxLkNvbnRlbnRCbG'
    '9ja1IHY29udGVudA==');

@$core.Deprecated('Use channelIngestRequestDescriptor instead')
const ChannelIngestRequest$json = {
  '1': 'ChannelIngestRequest',
  '2': [
    {'1': 'channel_id', '3': 1, '4': 1, '5': 9, '10': 'channelId'},
    {
      '1': 'user_message',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.UserMessage',
      '10': 'userMessage'
    },
  ],
};

/// Descriptor for `ChannelIngestRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelIngestRequestDescriptor = $convert.base64Decode(
    'ChRDaGFubmVsSW5nZXN0UmVxdWVzdBIdCgpjaGFubmVsX2lkGAEgASgJUgljaGFubmVsSWQSPA'
    'oMdXNlcl9tZXNzYWdlGAIgASgLMhkudGlnaHRiZWFtLnYxLlVzZXJNZXNzYWdlUgt1c2VyTWVz'
    'c2FnZQ==');

@$core.Deprecated('Use channelIngestAckDescriptor instead')
const ChannelIngestAck$json = {
  '1': 'ChannelIngestAck',
  '2': [
    {'1': 'channel_id', '3': 1, '4': 1, '5': 9, '10': 'channelId'},
    {'1': 'conversation_id', '3': 2, '4': 1, '5': 9, '10': 'conversationId'},
  ],
};

/// Descriptor for `ChannelIngestAck`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelIngestAckDescriptor = $convert.base64Decode(
    'ChBDaGFubmVsSW5nZXN0QWNrEh0KCmNoYW5uZWxfaWQYASABKAlSCWNoYW5uZWxJZBInCg9jb2'
    '52ZXJzYXRpb25faWQYAiABKAlSDmNvbnZlcnNhdGlvbklk');

@$core.Deprecated('Use channelReceiveRequestDescriptor instead')
const ChannelReceiveRequest$json = {
  '1': 'ChannelReceiveRequest',
  '2': [
    {
      '1': 'adapter_hint',
      '3': 1,
      '4': 1,
      '5': 9,
      '9': 0,
      '10': 'adapterHint',
      '17': true
    },
  ],
  '8': [
    {'1': '_adapter_hint'},
  ],
};

/// Descriptor for `ChannelReceiveRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelReceiveRequestDescriptor = $convert.base64Decode(
    'ChVDaGFubmVsUmVjZWl2ZVJlcXVlc3QSJgoMYWRhcHRlcl9oaW50GAEgASgJSABSC2FkYXB0ZX'
    'JIaW50iAEBQg8KDV9hZGFwdGVyX2hpbnQ=');

@$core.Deprecated('Use subscribeRequestDescriptor instead')
const SubscribeRequest$json = {
  '1': 'SubscribeRequest',
};

/// Descriptor for `SubscribeRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List subscribeRequestDescriptor =
    $convert.base64Decode('ChBTdWJzY3JpYmVSZXF1ZXN0');

@$core.Deprecated('Use userMessageDescriptor instead')
const UserMessage$json = {
  '1': 'UserMessage',
  '2': [
    {
      '1': 'content',
      '3': 1,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.ContentBlock',
      '10': 'content'
    },
    {'1': 'sender', '3': 2, '4': 1, '5': 9, '10': 'sender'},
    {
      '1': 'reply_channel',
      '3': 3,
      '4': 1,
      '5': 9,
      '9': 0,
      '10': 'replyChannel',
      '17': true
    },
  ],
  '8': [
    {'1': '_reply_channel'},
  ],
};

/// Descriptor for `UserMessage`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List userMessageDescriptor = $convert.base64Decode(
    'CgtVc2VyTWVzc2FnZRI0Cgdjb250ZW50GAEgAygLMhoudGlnaHRiZWFtLnYxLkNvbnRlbnRCbG'
    '9ja1IHY29udGVudBIWCgZzZW5kZXIYAiABKAlSBnNlbmRlchIoCg1yZXBseV9jaGFubmVsGAMg'
    'ASgJSABSDHJlcGx5Q2hhbm5lbIgBAUIQCg5fcmVwbHlfY2hhbm5lbA==');

@$core.Deprecated('Use redeemEnrollmentRequestDescriptor instead')
const RedeemEnrollmentRequest$json = {
  '1': 'RedeemEnrollmentRequest',
  '2': [
    {'1': 'enrollment_code', '3': 1, '4': 1, '5': 9, '10': 'enrollmentCode'},
    {'1': 'public_key', '3': 2, '4': 1, '5': 12, '10': 'publicKey'},
  ],
};

/// Descriptor for `RedeemEnrollmentRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List redeemEnrollmentRequestDescriptor =
    $convert.base64Decode(
        'ChdSZWRlZW1FbnJvbGxtZW50UmVxdWVzdBInCg9lbnJvbGxtZW50X2NvZGUYASABKAlSDmVucm'
        '9sbG1lbnRDb2RlEh0KCnB1YmxpY19rZXkYAiABKAxSCXB1YmxpY0tleQ==');

@$core.Deprecated('Use redeemEnrollmentResponseDescriptor instead')
const RedeemEnrollmentResponse$json = {
  '1': 'RedeemEnrollmentResponse',
  '2': [
    {'1': 'client_name', '3': 1, '4': 1, '5': 9, '10': 'clientName'},
    {'1': 'enrolled_at', '3': 2, '4': 1, '5': 3, '10': 'enrolledAt'},
  ],
};

/// Descriptor for `RedeemEnrollmentResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List redeemEnrollmentResponseDescriptor =
    $convert.base64Decode(
        'ChhSZWRlZW1FbnJvbGxtZW50UmVzcG9uc2USHwoLY2xpZW50X25hbWUYASABKAlSCmNsaWVudE'
        '5hbWUSHwoLZW5yb2xsZWRfYXQYAiABKANSCmVucm9sbGVkQXQ=');

@$core.Deprecated('Use getConversationHistoryRequestDescriptor instead')
const GetConversationHistoryRequest$json = {
  '1': 'GetConversationHistoryRequest',
  '2': [
    {'1': 'conversation_id', '3': 1, '4': 1, '5': 9, '10': 'conversationId'},
    {'1': 'limit', '3': 2, '4': 1, '5': 13, '9': 0, '10': 'limit', '17': true},
  ],
  '8': [
    {'1': '_limit'},
  ],
};

/// Descriptor for `GetConversationHistoryRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List getConversationHistoryRequestDescriptor =
    $convert.base64Decode(
        'Ch1HZXRDb252ZXJzYXRpb25IaXN0b3J5UmVxdWVzdBInCg9jb252ZXJzYXRpb25faWQYASABKA'
        'lSDmNvbnZlcnNhdGlvbklkEhkKBWxpbWl0GAIgASgNSABSBWxpbWl0iAEBQggKBl9saW1pdA==');

@$core.Deprecated('Use getConversationHistoryResponseDescriptor instead')
const GetConversationHistoryResponse$json = {
  '1': 'GetConversationHistoryResponse',
  '2': [
    {
      '1': 'entries',
      '3': 1,
      '4': 3,
      '5': 11,
      '6': '.tightbeam.v1.HistoryEntry',
      '10': 'entries'
    },
    {'1': 'total_seq', '3': 2, '4': 1, '5': 4, '10': 'totalSeq'},
    {'1': 'truncated', '3': 3, '4': 1, '5': 8, '10': 'truncated'},
  ],
};

/// Descriptor for `GetConversationHistoryResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List getConversationHistoryResponseDescriptor =
    $convert.base64Decode(
        'Ch5HZXRDb252ZXJzYXRpb25IaXN0b3J5UmVzcG9uc2USNAoHZW50cmllcxgBIAMoCzIaLnRpZ2'
        'h0YmVhbS52MS5IaXN0b3J5RW50cnlSB2VudHJpZXMSGwoJdG90YWxfc2VxGAIgASgEUgh0b3Rh'
        'bFNlcRIcCgl0cnVuY2F0ZWQYAyABKAhSCXRydW5jYXRlZA==');

@$core.Deprecated('Use historyEntryDescriptor instead')
const HistoryEntry$json = {
  '1': 'HistoryEntry',
  '2': [
    {'1': 'seq', '3': 1, '4': 1, '5': 4, '10': 'seq'},
    {'1': 'ts', '3': 2, '4': 1, '5': 9, '10': 'ts'},
    {
      '1': 'message',
      '3': 3,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.Message',
      '10': 'message'
    },
    {'1': 'tag', '3': 4, '4': 1, '5': 9, '9': 0, '10': 'tag', '17': true},
  ],
  '8': [
    {'1': '_tag'},
  ],
};

/// Descriptor for `HistoryEntry`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List historyEntryDescriptor = $convert.base64Decode(
    'CgxIaXN0b3J5RW50cnkSEAoDc2VxGAEgASgEUgNzZXESDgoCdHMYAiABKAlSAnRzEi8KB21lc3'
    'NhZ2UYAyABKAsyFS50aWdodGJlYW0udjEuTWVzc2FnZVIHbWVzc2FnZRIVCgN0YWcYBCABKAlI'
    'AFIDdGFniAEBQgYKBF90YWc=');
