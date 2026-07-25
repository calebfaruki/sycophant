// This is a generated file - do not edit.
//
// Generated from sycophant/common/v1/common.proto.

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

@$core.Deprecated('Use turnStateDescriptor instead')
const TurnState$json = {
  '1': 'TurnState',
  '2': [
    {'1': 'TURN_STATE_UNSPECIFIED', '2': 0},
    {'1': 'IDLE', '2': 1},
    {'1': 'WORKING', '2': 2},
    {'1': 'FAILED', '2': 5},
    {'1': 'CANCELLED', '2': 6},
  ],
};

/// Descriptor for `TurnState`. Decode as a `google.protobuf.EnumDescriptorProto`.
final $typed_data.Uint8List turnStateDescriptor = $convert.base64Decode(
    'CglUdXJuU3RhdGUSGgoWVFVSTl9TVEFURV9VTlNQRUNJRklFRBAAEggKBElETEUQARILCgdXT1'
    'JLSU5HEAISCgoGRkFJTEVEEAUSDQoJQ0FOQ0VMTEVEEAY=');

@$core.Deprecated('Use contentBlockDescriptor instead')
const ContentBlock$json = {
  '1': 'ContentBlock',
  '2': [
    {
      '1': 'text',
      '3': 1,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.TextBlock',
      '9': 0,
      '10': 'text'
    },
    {
      '1': 'image',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ImageBlock',
      '9': 0,
      '10': 'image'
    },
    {
      '1': 'thinking',
      '3': 3,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ThinkingBlock',
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
    'CgxDb250ZW50QmxvY2sSNAoEdGV4dBgBIAEoCzIeLnN5Y29waGFudC5jb21tb24udjEuVGV4dE'
    'Jsb2NrSABSBHRleHQSNwoFaW1hZ2UYAiABKAsyHy5zeWNvcGhhbnQuY29tbW9uLnYxLkltYWdl'
    'QmxvY2tIAFIFaW1hZ2USQAoIdGhpbmtpbmcYAyABKAsyIi5zeWNvcGhhbnQuY29tbW9uLnYxLl'
    'RoaW5raW5nQmxvY2tIAFIIdGhpbmtpbmdCBwoFYmxvY2s=');

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
      '6': '.sycophant.common.v1.ContentBlock',
      '10': 'content'
    },
    {
      '1': 'tool_calls',
      '3': 3,
      '4': 3,
      '5': 11,
      '6': '.sycophant.common.v1.ToolCall',
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
    'CgdNZXNzYWdlEhIKBHJvbGUYASABKAlSBHJvbGUSOwoHY29udGVudBgCIAMoCzIhLnN5Y29waG'
    'FudC5jb21tb24udjEuQ29udGVudEJsb2NrUgdjb250ZW50EjwKCnRvb2xfY2FsbHMYAyADKAsy'
    'HS5zeWNvcGhhbnQuY29tbW9uLnYxLlRvb2xDYWxsUgl0b29sQ2FsbHMSJQoMdG9vbF9jYWxsX2'
    'lkGAQgASgJSABSCnRvb2xDYWxsSWSIAQESHgoIaXNfZXJyb3IYBSABKAhIAVIHaXNFcnJvcogB'
    'AUIPCg1fdG9vbF9jYWxsX2lkQgsKCV9pc19lcnJvcg==');

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
    {
      '1': 'conversations',
      '3': 2,
      '4': 3,
      '5': 11,
      '6': '.sycophant.common.v1.ConversationSummary',
      '10': 'conversations'
    },
  ],
  '9': [
    {'1': 1, '2': 2},
  ],
  '10': ['conversation_ids'],
};

/// Descriptor for `ListConversationsResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List listConversationsResponseDescriptor = $convert.base64Decode(
    'ChlMaXN0Q29udmVyc2F0aW9uc1Jlc3BvbnNlEk4KDWNvbnZlcnNhdGlvbnMYAiADKAsyKC5zeW'
    'NvcGhhbnQuY29tbW9uLnYxLkNvbnZlcnNhdGlvblN1bW1hcnlSDWNvbnZlcnNhdGlvbnNKBAgB'
    'EAJSEGNvbnZlcnNhdGlvbl9pZHM=');

@$core.Deprecated('Use conversationSummaryDescriptor instead')
const ConversationSummary$json = {
  '1': 'ConversationSummary',
  '2': [
    {'1': 'conversation_id', '3': 1, '4': 1, '5': 9, '10': 'conversationId'},
    {
      '1': 'last_touched_ms_epoch',
      '3': 2,
      '4': 1,
      '5': 3,
      '10': 'lastTouchedMsEpoch'
    },
    {'1': 'name', '3': 3, '4': 1, '5': 9, '10': 'name'},
  ],
};

/// Descriptor for `ConversationSummary`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List conversationSummaryDescriptor = $convert.base64Decode(
    'ChNDb252ZXJzYXRpb25TdW1tYXJ5EicKD2NvbnZlcnNhdGlvbl9pZBgBIAEoCVIOY29udmVyc2'
    'F0aW9uSWQSMQoVbGFzdF90b3VjaGVkX21zX2Vwb2NoGAIgASgDUhJsYXN0VG91Y2hlZE1zRXBv'
    'Y2gSEgoEbmFtZRgDIAEoCVIEbmFtZQ==');

@$core.Deprecated('Use deleteConversationRequestDescriptor instead')
const DeleteConversationRequest$json = {
  '1': 'DeleteConversationRequest',
  '2': [
    {'1': 'conversation_id', '3': 1, '4': 1, '5': 9, '10': 'conversationId'},
  ],
};

/// Descriptor for `DeleteConversationRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List deleteConversationRequestDescriptor =
    $convert.base64Decode(
        'ChlEZWxldGVDb252ZXJzYXRpb25SZXF1ZXN0EicKD2NvbnZlcnNhdGlvbl9pZBgBIAEoCVIOY2'
        '9udmVyc2F0aW9uSWQ=');

@$core.Deprecated('Use deleteConversationResponseDescriptor instead')
const DeleteConversationResponse$json = {
  '1': 'DeleteConversationResponse',
};

/// Descriptor for `DeleteConversationResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List deleteConversationResponseDescriptor =
    $convert.base64Decode('ChpEZWxldGVDb252ZXJzYXRpb25SZXNwb25zZQ==');

@$core.Deprecated('Use setConversationNameRequestDescriptor instead')
const SetConversationNameRequest$json = {
  '1': 'SetConversationNameRequest',
  '2': [
    {'1': 'conversation_id', '3': 1, '4': 1, '5': 9, '10': 'conversationId'},
    {'1': 'name', '3': 2, '4': 1, '5': 9, '10': 'name'},
  ],
};

/// Descriptor for `SetConversationNameRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List setConversationNameRequestDescriptor =
    $convert.base64Decode(
        'ChpTZXRDb252ZXJzYXRpb25OYW1lUmVxdWVzdBInCg9jb252ZXJzYXRpb25faWQYASABKAlSDm'
        'NvbnZlcnNhdGlvbklkEhIKBG5hbWUYAiABKAlSBG5hbWU=');

@$core.Deprecated('Use setConversationNameResponseDescriptor instead')
const SetConversationNameResponse$json = {
  '1': 'SetConversationNameResponse',
};

/// Descriptor for `SetConversationNameResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List setConversationNameResponseDescriptor =
    $convert.base64Decode('ChtTZXRDb252ZXJzYXRpb25OYW1lUmVzcG9uc2U=');

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
      '6': '.sycophant.common.v1.ChannelAck',
      '9': 0,
      '10': 'ack'
    },
    {
      '1': 'send_message',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ChannelSend',
      '9': 0,
      '10': 'sendMessage'
    },
    {
      '1': 'turn_state',
      '3': 3,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.TurnStateEvent',
      '9': 0,
      '10': 'turnState'
    },
    {
      '1': 'server_request',
      '3': 4,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ServerRequest',
      '9': 0,
      '10': 'serverRequest'
    },
    {
      '1': 'stream_item',
      '3': 5,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.StreamItem',
      '9': 0,
      '10': 'streamItem'
    },
  ],
  '8': [
    {'1': 'command'},
  ],
};

/// Descriptor for `ChannelOutbound`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelOutboundDescriptor = $convert.base64Decode(
    'Cg9DaGFubmVsT3V0Ym91bmQSMwoDYWNrGAEgASgLMh8uc3ljb3BoYW50LmNvbW1vbi52MS5DaG'
    'FubmVsQWNrSABSA2FjaxJFCgxzZW5kX21lc3NhZ2UYAiABKAsyIC5zeWNvcGhhbnQuY29tbW9u'
    'LnYxLkNoYW5uZWxTZW5kSABSC3NlbmRNZXNzYWdlEkQKCnR1cm5fc3RhdGUYAyABKAsyIy5zeW'
    'NvcGhhbnQuY29tbW9uLnYxLlR1cm5TdGF0ZUV2ZW50SABSCXR1cm5TdGF0ZRJLCg5zZXJ2ZXJf'
    'cmVxdWVzdBgEIAEoCzIiLnN5Y29waGFudC5jb21tb24udjEuU2VydmVyUmVxdWVzdEgAUg1zZX'
    'J2ZXJSZXF1ZXN0EkIKC3N0cmVhbV9pdGVtGAUgASgLMh8uc3ljb3BoYW50LmNvbW1vbi52MS5T'
    'dHJlYW1JdGVtSABSCnN0cmVhbUl0ZW1CCQoHY29tbWFuZA==');

@$core.Deprecated('Use streamItemDescriptor instead')
const StreamItem$json = {
  '1': 'StreamItem',
  '2': [
    {'1': 'workspace_seq', '3': 1, '4': 1, '5': 4, '10': 'workspaceSeq'},
    {'1': 'event_id', '3': 2, '4': 1, '5': 9, '10': 'eventId'},
    {'1': 'item_id', '3': 3, '4': 1, '5': 9, '10': 'itemId'},
    {'1': 'conversation_id', '3': 4, '4': 1, '5': 9, '10': 'conversationId'},
    {
      '1': 'start',
      '3': 5,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ItemStart',
      '9': 0,
      '10': 'start'
    },
    {
      '1': 'delta',
      '3': 6,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ItemDelta',
      '9': 0,
      '10': 'delta'
    },
    {
      '1': 'stop',
      '3': 7,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ItemStop',
      '9': 0,
      '10': 'stop'
    },
    {
      '1': 'parent_conversation_id',
      '3': 8,
      '4': 1,
      '5': 9,
      '10': 'parentConversationId'
    },
    {'1': 'agent_name', '3': 9, '4': 1, '5': 9, '10': 'agentName'},
  ],
  '8': [
    {'1': 'phase'},
  ],
};

/// Descriptor for `StreamItem`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List streamItemDescriptor = $convert.base64Decode(
    'CgpTdHJlYW1JdGVtEiMKDXdvcmtzcGFjZV9zZXEYASABKARSDHdvcmtzcGFjZVNlcRIZCghldm'
    'VudF9pZBgCIAEoCVIHZXZlbnRJZBIXCgdpdGVtX2lkGAMgASgJUgZpdGVtSWQSJwoPY29udmVy'
    'c2F0aW9uX2lkGAQgASgJUg5jb252ZXJzYXRpb25JZBI2CgVzdGFydBgFIAEoCzIeLnN5Y29waG'
    'FudC5jb21tb24udjEuSXRlbVN0YXJ0SABSBXN0YXJ0EjYKBWRlbHRhGAYgASgLMh4uc3ljb3Bo'
    'YW50LmNvbW1vbi52MS5JdGVtRGVsdGFIAFIFZGVsdGESMwoEc3RvcBgHIAEoCzIdLnN5Y29waG'
    'FudC5jb21tb24udjEuSXRlbVN0b3BIAFIEc3RvcBI0ChZwYXJlbnRfY29udmVyc2F0aW9uX2lk'
    'GAggASgJUhRwYXJlbnRDb252ZXJzYXRpb25JZBIdCgphZ2VudF9uYW1lGAkgASgJUglhZ2VudE'
    '5hbWVCBwoFcGhhc2U=');

@$core.Deprecated('Use itemStartDescriptor instead')
const ItemStart$json = {
  '1': 'ItemStart',
  '2': [
    {
      '1': 'text',
      '3': 1,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.TextItem',
      '9': 0,
      '10': 'text'
    },
    {
      '1': 'tool_use',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ToolUseItem',
      '9': 0,
      '10': 'toolUse'
    },
  ],
  '8': [
    {'1': 'kind'},
  ],
};

/// Descriptor for `ItemStart`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List itemStartDescriptor = $convert.base64Decode(
    'CglJdGVtU3RhcnQSMwoEdGV4dBgBIAEoCzIdLnN5Y29waGFudC5jb21tb24udjEuVGV4dEl0ZW'
    '1IAFIEdGV4dBI9Cgh0b29sX3VzZRgCIAEoCzIgLnN5Y29waGFudC5jb21tb24udjEuVG9vbFVz'
    'ZUl0ZW1IAFIHdG9vbFVzZUIGCgRraW5k');

@$core.Deprecated('Use textItemDescriptor instead')
const TextItem$json = {
  '1': 'TextItem',
};

/// Descriptor for `TextItem`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List textItemDescriptor =
    $convert.base64Decode('CghUZXh0SXRlbQ==');

@$core.Deprecated('Use toolUseItemDescriptor instead')
const ToolUseItem$json = {
  '1': 'ToolUseItem',
  '2': [
    {'1': 'name', '3': 1, '4': 1, '5': 9, '10': 'name'},
  ],
};

/// Descriptor for `ToolUseItem`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List toolUseItemDescriptor =
    $convert.base64Decode('CgtUb29sVXNlSXRlbRISCgRuYW1lGAEgASgJUgRuYW1l');

@$core.Deprecated('Use itemDeltaDescriptor instead')
const ItemDelta$json = {
  '1': 'ItemDelta',
  '2': [
    {'1': 'text_delta', '3': 1, '4': 1, '5': 9, '9': 0, '10': 'textDelta'},
    {
      '1': 'tool_input_json',
      '3': 2,
      '4': 1,
      '5': 9,
      '9': 0,
      '10': 'toolInputJson'
    },
  ],
  '8': [
    {'1': 'kind'},
  ],
};

/// Descriptor for `ItemDelta`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List itemDeltaDescriptor = $convert.base64Decode(
    'CglJdGVtRGVsdGESHwoKdGV4dF9kZWx0YRgBIAEoCUgAUgl0ZXh0RGVsdGESKAoPdG9vbF9pbn'
    'B1dF9qc29uGAIgASgJSABSDXRvb2xJbnB1dEpzb25CBgoEa2luZA==');

@$core.Deprecated('Use itemStopDescriptor instead')
const ItemStop$json = {
  '1': 'ItemStop',
};

/// Descriptor for `ItemStop`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List itemStopDescriptor =
    $convert.base64Decode('CghJdGVtU3RvcA==');

@$core.Deprecated('Use channelSendDescriptor instead')
const ChannelSend$json = {
  '1': 'ChannelSend',
  '2': [
    {
      '1': 'content',
      '3': 1,
      '4': 3,
      '5': 11,
      '6': '.sycophant.common.v1.ContentBlock',
      '10': 'content'
    },
    {'1': 'conversation_id', '3': 2, '4': 1, '5': 9, '10': 'conversationId'},
  ],
};

/// Descriptor for `ChannelSend`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelSendDescriptor = $convert.base64Decode(
    'CgtDaGFubmVsU2VuZBI7Cgdjb250ZW50GAEgAygLMiEuc3ljb3BoYW50LmNvbW1vbi52MS5Db2'
    '50ZW50QmxvY2tSB2NvbnRlbnQSJwoPY29udmVyc2F0aW9uX2lkGAIgASgJUg5jb252ZXJzYXRp'
    'b25JZA==');

@$core.Deprecated('Use turnStateEventDescriptor instead')
const TurnStateEvent$json = {
  '1': 'TurnStateEvent',
  '2': [
    {
      '1': 'state',
      '3': 1,
      '4': 1,
      '5': 14,
      '6': '.sycophant.common.v1.TurnState',
      '10': 'state'
    },
    {'1': 'conversation_id', '3': 2, '4': 1, '5': 9, '10': 'conversationId'},
    {'1': 'reason', '3': 3, '4': 1, '5': 9, '10': 'reason'},
    {'1': 'code', '3': 4, '4': 1, '5': 9, '10': 'code'},
    {'1': 'agent_name', '3': 5, '4': 1, '5': 9, '10': 'agentName'},
    {
      '1': 'system_prompt_sha256',
      '3': 6,
      '4': 1,
      '5': 9,
      '10': 'systemPromptSha256'
    },
  ],
};

/// Descriptor for `TurnStateEvent`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List turnStateEventDescriptor = $convert.base64Decode(
    'Cg5UdXJuU3RhdGVFdmVudBI0CgVzdGF0ZRgBIAEoDjIeLnN5Y29waGFudC5jb21tb24udjEuVH'
    'VyblN0YXRlUgVzdGF0ZRInCg9jb252ZXJzYXRpb25faWQYAiABKAlSDmNvbnZlcnNhdGlvbklk'
    'EhYKBnJlYXNvbhgDIAEoCVIGcmVhc29uEhIKBGNvZGUYBCABKAlSBGNvZGUSHQoKYWdlbnRfbm'
    'FtZRgFIAEoCVIJYWdlbnROYW1lEjAKFHN5c3RlbV9wcm9tcHRfc2hhMjU2GAYgASgJUhJzeXN0'
    'ZW1Qcm9tcHRTaGEyNTY=');

@$core.Deprecated('Use userMessageDescriptor instead')
const UserMessage$json = {
  '1': 'UserMessage',
  '2': [
    {
      '1': 'content',
      '3': 1,
      '4': 3,
      '5': 11,
      '6': '.sycophant.common.v1.ContentBlock',
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
    {'1': 'conversation_id', '3': 4, '4': 1, '5': 9, '10': 'conversationId'},
  ],
  '8': [
    {'1': '_reply_channel'},
  ],
};

/// Descriptor for `UserMessage`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List userMessageDescriptor = $convert.base64Decode(
    'CgtVc2VyTWVzc2FnZRI7Cgdjb250ZW50GAEgAygLMiEuc3ljb3BoYW50LmNvbW1vbi52MS5Db2'
    '50ZW50QmxvY2tSB2NvbnRlbnQSFgoGc2VuZGVyGAIgASgJUgZzZW5kZXISKAoNcmVwbHlfY2hh'
    'bm5lbBgDIAEoCUgAUgxyZXBseUNoYW5uZWyIAQESJwoPY29udmVyc2F0aW9uX2lkGAQgASgJUg'
    '5jb252ZXJzYXRpb25JZEIQCg5fcmVwbHlfY2hhbm5lbA==');

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
      '6': '.sycophant.common.v1.HistoryEntry',
      '10': 'entries'
    },
    {'1': 'total_seq', '3': 2, '4': 1, '5': 4, '10': 'totalSeq'},
    {'1': 'truncated', '3': 3, '4': 1, '5': 8, '10': 'truncated'},
  ],
};

/// Descriptor for `GetConversationHistoryResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List getConversationHistoryResponseDescriptor =
    $convert.base64Decode(
        'Ch5HZXRDb252ZXJzYXRpb25IaXN0b3J5UmVzcG9uc2USOwoHZW50cmllcxgBIAMoCzIhLnN5Y2'
        '9waGFudC5jb21tb24udjEuSGlzdG9yeUVudHJ5UgdlbnRyaWVzEhsKCXRvdGFsX3NlcRgCIAEo'
        'BFIIdG90YWxTZXESHAoJdHJ1bmNhdGVkGAMgASgIUgl0cnVuY2F0ZWQ=');

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
      '6': '.sycophant.common.v1.Message',
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
    'CgxIaXN0b3J5RW50cnkSEAoDc2VxGAEgASgEUgNzZXESDgoCdHMYAiABKAlSAnRzEjYKB21lc3'
    'NhZ2UYAyABKAsyHC5zeWNvcGhhbnQuY29tbW9uLnYxLk1lc3NhZ2VSB21lc3NhZ2USFQoDdGFn'
    'GAQgASgJSABSA3RhZ4gBAUIGCgRfdGFn');

@$core.Deprecated('Use getTurnStateRequestDescriptor instead')
const GetTurnStateRequest$json = {
  '1': 'GetTurnStateRequest',
  '2': [
    {'1': 'conversation_id', '3': 1, '4': 1, '5': 9, '10': 'conversationId'},
  ],
};

/// Descriptor for `GetTurnStateRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List getTurnStateRequestDescriptor = $convert.base64Decode(
    'ChNHZXRUdXJuU3RhdGVSZXF1ZXN0EicKD2NvbnZlcnNhdGlvbl9pZBgBIAEoCVIOY29udmVyc2'
    'F0aW9uSWQ=');

@$core.Deprecated('Use cancelTurnRequestDescriptor instead')
const CancelTurnRequest$json = {
  '1': 'CancelTurnRequest',
  '2': [
    {'1': 'conversation_id', '3': 1, '4': 1, '5': 9, '10': 'conversationId'},
  ],
};

/// Descriptor for `CancelTurnRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List cancelTurnRequestDescriptor = $convert.base64Decode(
    'ChFDYW5jZWxUdXJuUmVxdWVzdBInCg9jb252ZXJzYXRpb25faWQYASABKAlSDmNvbnZlcnNhdG'
    'lvbklk');

@$core.Deprecated('Use cancelTurnResponseDescriptor instead')
const CancelTurnResponse$json = {
  '1': 'CancelTurnResponse',
  '2': [
    {'1': 'cancelled', '3': 1, '4': 1, '5': 8, '10': 'cancelled'},
  ],
};

/// Descriptor for `CancelTurnResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List cancelTurnResponseDescriptor =
    $convert.base64Decode(
        'ChJDYW5jZWxUdXJuUmVzcG9uc2USHAoJY2FuY2VsbGVkGAEgASgIUgljYW5jZWxsZWQ=');

@$core.Deprecated('Use watchToolsRequestDescriptor instead')
const WatchToolsRequest$json = {
  '1': 'WatchToolsRequest',
};

/// Descriptor for `WatchToolsRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List watchToolsRequestDescriptor =
    $convert.base64Decode('ChFXYXRjaFRvb2xzUmVxdWVzdA==');

@$core.Deprecated('Use toolListUpdateDescriptor instead')
const ToolListUpdate$json = {
  '1': 'ToolListUpdate',
  '2': [
    {
      '1': 'tools',
      '3': 1,
      '4': 3,
      '5': 11,
      '6': '.sycophant.common.v1.ToolInfo',
      '10': 'tools'
    },
  ],
};

/// Descriptor for `ToolListUpdate`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List toolListUpdateDescriptor = $convert.base64Decode(
    'Cg5Ub29sTGlzdFVwZGF0ZRIzCgV0b29scxgBIAMoCzIdLnN5Y29waGFudC5jb21tb24udjEuVG'
    '9vbEluZm9SBXRvb2xz');

@$core.Deprecated('Use toolInfoDescriptor instead')
const ToolInfo$json = {
  '1': 'ToolInfo',
  '2': [
    {'1': 'name', '3': 1, '4': 1, '5': 9, '10': 'name'},
    {'1': 'description', '3': 2, '4': 1, '5': 9, '10': 'description'},
    {'1': 'parameters_json', '3': 3, '4': 1, '5': 9, '10': 'parametersJson'},
  ],
};

/// Descriptor for `ToolInfo`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List toolInfoDescriptor = $convert.base64Decode(
    'CghUb29sSW5mbxISCgRuYW1lGAEgASgJUgRuYW1lEiAKC2Rlc2NyaXB0aW9uGAIgASgJUgtkZX'
    'NjcmlwdGlvbhInCg9wYXJhbWV0ZXJzX2pzb24YAyABKAlSDnBhcmFtZXRlcnNKc29u');

@$core.Deprecated('Use callToolRequestDescriptor instead')
const CallToolRequest$json = {
  '1': 'CallToolRequest',
  '2': [
    {'1': 'name', '3': 1, '4': 1, '5': 9, '10': 'name'},
    {'1': 'input_json', '3': 2, '4': 1, '5': 9, '10': 'inputJson'},
  ],
};

/// Descriptor for `CallToolRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List callToolRequestDescriptor = $convert.base64Decode(
    'Cg9DYWxsVG9vbFJlcXVlc3QSEgoEbmFtZRgBIAEoCVIEbmFtZRIdCgppbnB1dF9qc29uGAIgAS'
    'gJUglpbnB1dEpzb24=');

@$core.Deprecated('Use callToolResponseDescriptor instead')
const CallToolResponse$json = {
  '1': 'CallToolResponse',
  '2': [
    {
      '1': 'content',
      '3': 1,
      '4': 3,
      '5': 11,
      '6': '.sycophant.common.v1.ContentBlock',
      '10': 'content'
    },
    {'1': 'is_error', '3': 2, '4': 1, '5': 8, '10': 'isError'},
  ],
};

/// Descriptor for `CallToolResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List callToolResponseDescriptor = $convert.base64Decode(
    'ChBDYWxsVG9vbFJlc3BvbnNlEjsKB2NvbnRlbnQYASADKAsyIS5zeWNvcGhhbnQuY29tbW9uLn'
    'YxLkNvbnRlbnRCbG9ja1IHY29udGVudBIZCghpc19lcnJvchgCIAEoCFIHaXNFcnJvcg==');

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
      '6': '.sycophant.common.v1.UserMessage',
      '10': 'userMessage'
    },
    {
      '1': 'client_response',
      '3': 3,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ClientResponse',
      '10': 'clientResponse'
    },
    {
      '1': 'supported_methods',
      '3': 4,
      '4': 3,
      '5': 9,
      '10': 'supportedMethods'
    },
    {'1': 'conversation_id', '3': 5, '4': 1, '5': 9, '10': 'conversationId'},
  ],
};

/// Descriptor for `ChannelIngestRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelIngestRequestDescriptor = $convert.base64Decode(
    'ChRDaGFubmVsSW5nZXN0UmVxdWVzdBIdCgpjaGFubmVsX2lkGAEgASgJUgljaGFubmVsSWQSQw'
    'oMdXNlcl9tZXNzYWdlGAIgASgLMiAuc3ljb3BoYW50LmNvbW1vbi52MS5Vc2VyTWVzc2FnZVIL'
    'dXNlck1lc3NhZ2USTAoPY2xpZW50X3Jlc3BvbnNlGAMgASgLMiMuc3ljb3BoYW50LmNvbW1vbi'
    '52MS5DbGllbnRSZXNwb25zZVIOY2xpZW50UmVzcG9uc2USKwoRc3VwcG9ydGVkX21ldGhvZHMY'
    'BCADKAlSEHN1cHBvcnRlZE1ldGhvZHMSJwoPY29udmVyc2F0aW9uX2lkGAUgASgJUg5jb252ZX'
    'JzYXRpb25JZA==');

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

@$core.Deprecated('Use serverRequestDescriptor instead')
const ServerRequest$json = {
  '1': 'ServerRequest',
  '2': [
    {'1': 'request_id', '3': 1, '4': 1, '5': 9, '10': 'requestId'},
    {'1': 'method', '3': 2, '4': 1, '5': 9, '10': 'method'},
    {'1': 'params_json', '3': 3, '4': 1, '5': 9, '10': 'paramsJson'},
  ],
};

/// Descriptor for `ServerRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List serverRequestDescriptor = $convert.base64Decode(
    'Cg1TZXJ2ZXJSZXF1ZXN0Eh0KCnJlcXVlc3RfaWQYASABKAlSCXJlcXVlc3RJZBIWCgZtZXRob2'
    'QYAiABKAlSBm1ldGhvZBIfCgtwYXJhbXNfanNvbhgDIAEoCVIKcGFyYW1zSnNvbg==');

@$core.Deprecated('Use clientResponseDescriptor instead')
const ClientResponse$json = {
  '1': 'ClientResponse',
  '2': [
    {'1': 'request_id', '3': 1, '4': 1, '5': 9, '10': 'requestId'},
    {'1': 'result_json', '3': 2, '4': 1, '5': 9, '10': 'resultJson'},
    {
      '1': 'error',
      '3': 3,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ClientResponseError',
      '10': 'error'
    },
  ],
};

/// Descriptor for `ClientResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List clientResponseDescriptor = $convert.base64Decode(
    'Cg5DbGllbnRSZXNwb25zZRIdCgpyZXF1ZXN0X2lkGAEgASgJUglyZXF1ZXN0SWQSHwoLcmVzdW'
    'x0X2pzb24YAiABKAlSCnJlc3VsdEpzb24SPgoFZXJyb3IYAyABKAsyKC5zeWNvcGhhbnQuY29t'
    'bW9uLnYxLkNsaWVudFJlc3BvbnNlRXJyb3JSBWVycm9y');

@$core.Deprecated('Use clientResponseErrorDescriptor instead')
const ClientResponseError$json = {
  '1': 'ClientResponseError',
  '2': [
    {'1': 'code', '3': 1, '4': 1, '5': 5, '10': 'code'},
    {'1': 'message', '3': 2, '4': 1, '5': 9, '10': 'message'},
  ],
};

/// Descriptor for `ClientResponseError`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List clientResponseErrorDescriptor = $convert.base64Decode(
    'ChNDbGllbnRSZXNwb25zZUVycm9yEhIKBGNvZGUYASABKAVSBGNvZGUSGAoHbWVzc2FnZRgCIA'
    'EoCVIHbWVzc2FnZQ==');

@$core.Deprecated('Use sendServerNotificationRequestDescriptor instead')
const SendServerNotificationRequest$json = {
  '1': 'SendServerNotificationRequest',
  '2': [
    {'1': 'channel_id', '3': 1, '4': 1, '5': 9, '10': 'channelId'},
    {'1': 'method', '3': 2, '4': 1, '5': 9, '10': 'method'},
    {'1': 'params_json', '3': 3, '4': 1, '5': 9, '10': 'paramsJson'},
  ],
};

/// Descriptor for `SendServerNotificationRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List sendServerNotificationRequestDescriptor =
    $convert.base64Decode(
        'Ch1TZW5kU2VydmVyTm90aWZpY2F0aW9uUmVxdWVzdBIdCgpjaGFubmVsX2lkGAEgASgJUgljaG'
        'FubmVsSWQSFgoGbWV0aG9kGAIgASgJUgZtZXRob2QSHwoLcGFyYW1zX2pzb24YAyABKAlSCnBh'
        'cmFtc0pzb24=');

@$core.Deprecated('Use sendServerNotificationResponseDescriptor instead')
const SendServerNotificationResponse$json = {
  '1': 'SendServerNotificationResponse',
  '2': [
    {'1': 'delivered', '3': 1, '4': 1, '5': 8, '10': 'delivered'},
  ],
};

/// Descriptor for `SendServerNotificationResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List sendServerNotificationResponseDescriptor =
    $convert.base64Decode(
        'Ch5TZW5kU2VydmVyTm90aWZpY2F0aW9uUmVzcG9uc2USHAoJZGVsaXZlcmVkGAEgASgIUglkZW'
        'xpdmVyZWQ=');

@$core.Deprecated('Use sendServerRequestAndAwaitRequestDescriptor instead')
const SendServerRequestAndAwaitRequest$json = {
  '1': 'SendServerRequestAndAwaitRequest',
  '2': [
    {'1': 'channel_id', '3': 1, '4': 1, '5': 9, '10': 'channelId'},
    {'1': 'request_id', '3': 2, '4': 1, '5': 9, '10': 'requestId'},
    {'1': 'method', '3': 3, '4': 1, '5': 9, '10': 'method'},
    {'1': 'params_json', '3': 4, '4': 1, '5': 9, '10': 'paramsJson'},
    {'1': 'timeout_seconds', '3': 5, '4': 1, '5': 13, '10': 'timeoutSeconds'},
  ],
};

/// Descriptor for `SendServerRequestAndAwaitRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List sendServerRequestAndAwaitRequestDescriptor =
    $convert.base64Decode(
        'CiBTZW5kU2VydmVyUmVxdWVzdEFuZEF3YWl0UmVxdWVzdBIdCgpjaGFubmVsX2lkGAEgASgJUg'
        'ljaGFubmVsSWQSHQoKcmVxdWVzdF9pZBgCIAEoCVIJcmVxdWVzdElkEhYKBm1ldGhvZBgDIAEo'
        'CVIGbWV0aG9kEh8KC3BhcmFtc19qc29uGAQgASgJUgpwYXJhbXNKc29uEicKD3RpbWVvdXRfc2'
        'Vjb25kcxgFIAEoDVIOdGltZW91dFNlY29uZHM=');

@$core.Deprecated('Use sendServerRequestAndAwaitResponseDescriptor instead')
const SendServerRequestAndAwaitResponse$json = {
  '1': 'SendServerRequestAndAwaitResponse',
  '2': [
    {'1': 'result_json', '3': 1, '4': 1, '5': 9, '10': 'resultJson'},
    {
      '1': 'error',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.ClientResponseError',
      '10': 'error'
    },
    {'1': 'timed_out', '3': 3, '4': 1, '5': 8, '10': 'timedOut'},
    {'1': 'unknown_channel', '3': 4, '4': 1, '5': 8, '10': 'unknownChannel'},
    {
      '1': 'unsupported_method',
      '3': 5,
      '4': 1,
      '5': 8,
      '10': 'unsupportedMethod'
    },
  ],
};

/// Descriptor for `SendServerRequestAndAwaitResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List sendServerRequestAndAwaitResponseDescriptor = $convert.base64Decode(
    'CiFTZW5kU2VydmVyUmVxdWVzdEFuZEF3YWl0UmVzcG9uc2USHwoLcmVzdWx0X2pzb24YASABKA'
    'lSCnJlc3VsdEpzb24SPgoFZXJyb3IYAiABKAsyKC5zeWNvcGhhbnQuY29tbW9uLnYxLkNsaWVu'
    'dFJlc3BvbnNlRXJyb3JSBWVycm9yEhsKCXRpbWVkX291dBgDIAEoCFIIdGltZWRPdXQSJwoPdW'
    '5rbm93bl9jaGFubmVsGAQgASgIUg51bmtub3duQ2hhbm5lbBItChJ1bnN1cHBvcnRlZF9tZXRo'
    'b2QYBSABKAhSEXVuc3VwcG9ydGVkTWV0aG9k');
