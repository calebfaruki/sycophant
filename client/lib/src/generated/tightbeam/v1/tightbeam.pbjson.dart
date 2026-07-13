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

@$core.Deprecated('Use deliverStreamItemRequestDescriptor instead')
const DeliverStreamItemRequest$json = {
  '1': 'DeliverStreamItemRequest',
  '2': [
    {'1': 'channel_id', '3': 1, '4': 1, '5': 9, '10': 'channelId'},
    {
      '1': 'item',
      '3': 2,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.StreamItem',
      '10': 'item'
    },
  ],
};

/// Descriptor for `DeliverStreamItemRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List deliverStreamItemRequestDescriptor = $convert.base64Decode(
    'ChhEZWxpdmVyU3RyZWFtSXRlbVJlcXVlc3QSHQoKY2hhbm5lbF9pZBgBIAEoCVIJY2hhbm5lbE'
    'lkEjMKBGl0ZW0YAiABKAsyHy5zeWNvcGhhbnQuY29tbW9uLnYxLlN0cmVhbUl0ZW1SBGl0ZW0=');

@$core.Deprecated('Use deliverStreamItemResponseDescriptor instead')
const DeliverStreamItemResponse$json = {
  '1': 'DeliverStreamItemResponse',
  '2': [
    {'1': 'delivered', '3': 1, '4': 1, '5': 8, '10': 'delivered'},
  ],
};

/// Descriptor for `DeliverStreamItemResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List deliverStreamItemResponseDescriptor =
    $convert.base64Decode(
        'ChlEZWxpdmVyU3RyZWFtSXRlbVJlc3BvbnNlEhwKCWRlbGl2ZXJlZBgBIAEoCFIJZGVsaXZlcm'
        'Vk');

@$core.Deprecated('Use deliverOutboundRequestDescriptor instead')
const DeliverOutboundRequest$json = {
  '1': 'DeliverOutboundRequest',
  '2': [
    {'1': 'channel_id', '3': 1, '4': 1, '5': 9, '10': 'channelId'},
    {'1': 'conversation_id', '3': 2, '4': 1, '5': 9, '10': 'conversationId'},
    {
      '1': 'reply',
      '3': 3,
      '4': 1,
      '5': 11,
      '6': '.tightbeam.v1.ChannelReply',
      '9': 0,
      '10': 'reply',
      '17': true
    },
    {
      '1': 'turn_state',
      '3': 4,
      '4': 1,
      '5': 11,
      '6': '.sycophant.common.v1.TurnStateEvent',
      '10': 'turnState'
    },
  ],
  '8': [
    {'1': '_reply'},
  ],
};

/// Descriptor for `DeliverOutboundRequest`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List deliverOutboundRequestDescriptor = $convert.base64Decode(
    'ChZEZWxpdmVyT3V0Ym91bmRSZXF1ZXN0Eh0KCmNoYW5uZWxfaWQYASABKAlSCWNoYW5uZWxJZB'
    'InCg9jb252ZXJzYXRpb25faWQYAiABKAlSDmNvbnZlcnNhdGlvbklkEjUKBXJlcGx5GAMgASgL'
    'MhoudGlnaHRiZWFtLnYxLkNoYW5uZWxSZXBseUgAUgVyZXBseYgBARJCCgp0dXJuX3N0YXRlGA'
    'QgASgLMiMuc3ljb3BoYW50LmNvbW1vbi52MS5UdXJuU3RhdGVFdmVudFIJdHVyblN0YXRlQggK'
    'Bl9yZXBseQ==');

@$core.Deprecated('Use channelReplyDescriptor instead')
const ChannelReply$json = {
  '1': 'ChannelReply',
  '2': [
    {
      '1': 'content',
      '3': 1,
      '4': 3,
      '5': 11,
      '6': '.sycophant.common.v1.ContentBlock',
      '10': 'content'
    },
  ],
};

/// Descriptor for `ChannelReply`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List channelReplyDescriptor = $convert.base64Decode(
    'CgxDaGFubmVsUmVwbHkSOwoHY29udGVudBgBIAMoCzIhLnN5Y29waGFudC5jb21tb24udjEuQ2'
    '9udGVudEJsb2NrUgdjb250ZW50');

@$core.Deprecated('Use deliverOutboundResponseDescriptor instead')
const DeliverOutboundResponse$json = {
  '1': 'DeliverOutboundResponse',
  '2': [
    {'1': 'delivered', '3': 1, '4': 1, '5': 8, '10': 'delivered'},
  ],
};

/// Descriptor for `DeliverOutboundResponse`. Decode as a `google.protobuf.DescriptorProto`.
final $typed_data.Uint8List deliverOutboundResponseDescriptor =
    $convert.base64Decode(
        'ChdEZWxpdmVyT3V0Ym91bmRSZXNwb25zZRIcCglkZWxpdmVyZWQYASABKAhSCWRlbGl2ZXJlZA'
        '==');
