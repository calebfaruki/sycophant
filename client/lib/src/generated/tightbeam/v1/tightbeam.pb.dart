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

import '../../sycophant/common/v1/common.pb.dart' as $0;

export 'package:protobuf/protobuf.dart' show GeneratedMessageGenericExtensions;

class DeliverStreamItemRequest extends $pb.GeneratedMessage {
  factory DeliverStreamItemRequest({
    $core.String? channelId,
    $0.StreamItem? item,
  }) {
    final result = create();
    if (channelId != null) result.channelId = channelId;
    if (item != null) result.item = item;
    return result;
  }

  DeliverStreamItemRequest._();

  factory DeliverStreamItemRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory DeliverStreamItemRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'DeliverStreamItemRequest',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'channelId')
    ..aOM<$0.StreamItem>(2, _omitFieldNames ? '' : 'item',
        subBuilder: $0.StreamItem.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeliverStreamItemRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeliverStreamItemRequest copyWith(
          void Function(DeliverStreamItemRequest) updates) =>
      super.copyWith((message) => updates(message as DeliverStreamItemRequest))
          as DeliverStreamItemRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static DeliverStreamItemRequest create() => DeliverStreamItemRequest._();
  @$core.override
  DeliverStreamItemRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static DeliverStreamItemRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<DeliverStreamItemRequest>(create);
  static DeliverStreamItemRequest? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get channelId => $_getSZ(0);
  @$pb.TagNumber(1)
  set channelId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasChannelId() => $_has(0);
  @$pb.TagNumber(1)
  void clearChannelId() => $_clearField(1);

  @$pb.TagNumber(2)
  $0.StreamItem get item => $_getN(1);
  @$pb.TagNumber(2)
  set item($0.StreamItem value) => $_setField(2, value);
  @$pb.TagNumber(2)
  $core.bool hasItem() => $_has(1);
  @$pb.TagNumber(2)
  void clearItem() => $_clearField(2);
  @$pb.TagNumber(2)
  $0.StreamItem ensureItem() => $_ensure(1);
}

class DeliverStreamItemResponse extends $pb.GeneratedMessage {
  factory DeliverStreamItemResponse({
    $core.bool? delivered,
  }) {
    final result = create();
    if (delivered != null) result.delivered = delivered;
    return result;
  }

  DeliverStreamItemResponse._();

  factory DeliverStreamItemResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory DeliverStreamItemResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'DeliverStreamItemResponse',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOB(1, _omitFieldNames ? '' : 'delivered')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeliverStreamItemResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeliverStreamItemResponse copyWith(
          void Function(DeliverStreamItemResponse) updates) =>
      super.copyWith((message) => updates(message as DeliverStreamItemResponse))
          as DeliverStreamItemResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static DeliverStreamItemResponse create() => DeliverStreamItemResponse._();
  @$core.override
  DeliverStreamItemResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static DeliverStreamItemResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<DeliverStreamItemResponse>(create);
  static DeliverStreamItemResponse? _defaultInstance;

  @$pb.TagNumber(1)
  $core.bool get delivered => $_getBF(0);
  @$pb.TagNumber(1)
  set delivered($core.bool value) => $_setBool(0, value);
  @$pb.TagNumber(1)
  $core.bool hasDelivered() => $_has(0);
  @$pb.TagNumber(1)
  void clearDelivered() => $_clearField(1);
}

class DeliverOutboundRequest extends $pb.GeneratedMessage {
  factory DeliverOutboundRequest({
    $core.String? channelId,
    $core.String? conversationId,
    ChannelReply? reply,
    $0.TurnStateEvent? turnState,
  }) {
    final result = create();
    if (channelId != null) result.channelId = channelId;
    if (conversationId != null) result.conversationId = conversationId;
    if (reply != null) result.reply = reply;
    if (turnState != null) result.turnState = turnState;
    return result;
  }

  DeliverOutboundRequest._();

  factory DeliverOutboundRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory DeliverOutboundRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'DeliverOutboundRequest',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'channelId')
    ..aOS(2, _omitFieldNames ? '' : 'conversationId')
    ..aOM<ChannelReply>(3, _omitFieldNames ? '' : 'reply',
        subBuilder: ChannelReply.create)
    ..aOM<$0.TurnStateEvent>(4, _omitFieldNames ? '' : 'turnState',
        subBuilder: $0.TurnStateEvent.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeliverOutboundRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeliverOutboundRequest copyWith(
          void Function(DeliverOutboundRequest) updates) =>
      super.copyWith((message) => updates(message as DeliverOutboundRequest))
          as DeliverOutboundRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static DeliverOutboundRequest create() => DeliverOutboundRequest._();
  @$core.override
  DeliverOutboundRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static DeliverOutboundRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<DeliverOutboundRequest>(create);
  static DeliverOutboundRequest? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get channelId => $_getSZ(0);
  @$pb.TagNumber(1)
  set channelId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasChannelId() => $_has(0);
  @$pb.TagNumber(1)
  void clearChannelId() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get conversationId => $_getSZ(1);
  @$pb.TagNumber(2)
  set conversationId($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasConversationId() => $_has(1);
  @$pb.TagNumber(2)
  void clearConversationId() => $_clearField(2);

  /// present => enqueue reply BEFORE turn_state
  @$pb.TagNumber(3)
  ChannelReply get reply => $_getN(2);
  @$pb.TagNumber(3)
  set reply(ChannelReply value) => $_setField(3, value);
  @$pb.TagNumber(3)
  $core.bool hasReply() => $_has(2);
  @$pb.TagNumber(3)
  void clearReply() => $_clearField(3);
  @$pb.TagNumber(3)
  ChannelReply ensureReply() => $_ensure(2);

  @$pb.TagNumber(4)
  $0.TurnStateEvent get turnState => $_getN(3);
  @$pb.TagNumber(4)
  set turnState($0.TurnStateEvent value) => $_setField(4, value);
  @$pb.TagNumber(4)
  $core.bool hasTurnState() => $_has(3);
  @$pb.TagNumber(4)
  void clearTurnState() => $_clearField(4);
  @$pb.TagNumber(4)
  $0.TurnStateEvent ensureTurnState() => $_ensure(3);
}

class ChannelReply extends $pb.GeneratedMessage {
  factory ChannelReply({
    $core.Iterable<$0.ContentBlock>? content,
  }) {
    final result = create();
    if (content != null) result.content.addAll(content);
    return result;
  }

  ChannelReply._();

  factory ChannelReply.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ChannelReply.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ChannelReply',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..pPM<$0.ContentBlock>(1, _omitFieldNames ? '' : 'content',
        subBuilder: $0.ContentBlock.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelReply clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelReply copyWith(void Function(ChannelReply) updates) =>
      super.copyWith((message) => updates(message as ChannelReply))
          as ChannelReply;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ChannelReply create() => ChannelReply._();
  @$core.override
  ChannelReply createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ChannelReply getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ChannelReply>(create);
  static ChannelReply? _defaultInstance;

  @$pb.TagNumber(1)
  $pb.PbList<$0.ContentBlock> get content => $_getList(0);
}

class DeliverOutboundResponse extends $pb.GeneratedMessage {
  factory DeliverOutboundResponse({
    $core.bool? delivered,
  }) {
    final result = create();
    if (delivered != null) result.delivered = delivered;
    return result;
  }

  DeliverOutboundResponse._();

  factory DeliverOutboundResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory DeliverOutboundResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'DeliverOutboundResponse',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOB(1, _omitFieldNames ? '' : 'delivered')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeliverOutboundResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeliverOutboundResponse copyWith(
          void Function(DeliverOutboundResponse) updates) =>
      super.copyWith((message) => updates(message as DeliverOutboundResponse))
          as DeliverOutboundResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static DeliverOutboundResponse create() => DeliverOutboundResponse._();
  @$core.override
  DeliverOutboundResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static DeliverOutboundResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<DeliverOutboundResponse>(create);
  static DeliverOutboundResponse? _defaultInstance;

  @$pb.TagNumber(1)
  $core.bool get delivered => $_getBF(0);
  @$pb.TagNumber(1)
  set delivered($core.bool value) => $_setBool(0, value);
  @$pb.TagNumber(1)
  $core.bool hasDelivered() => $_has(0);
  @$pb.TagNumber(1)
  void clearDelivered() => $_clearField(1);
}

const $core.bool _omitFieldNames =
    $core.bool.fromEnvironment('protobuf.omit_field_names');
const $core.bool _omitMessageNames =
    $core.bool.fromEnvironment('protobuf.omit_message_names');
