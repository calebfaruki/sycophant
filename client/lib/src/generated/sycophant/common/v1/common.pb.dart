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

import 'package:fixnum/fixnum.dart' as $fixnum;
import 'package:protobuf/protobuf.dart' as $pb;

import 'common.pbenum.dart';

export 'package:protobuf/protobuf.dart' show GeneratedMessageGenericExtensions;

export 'common.pbenum.dart';

enum ContentBlock_Block { text, image, thinking, notSet }

class ContentBlock extends $pb.GeneratedMessage {
  factory ContentBlock({
    TextBlock? text,
    ImageBlock? image,
    ThinkingBlock? thinking,
  }) {
    final result = create();
    if (text != null) result.text = text;
    if (image != null) result.image = image;
    if (thinking != null) result.thinking = thinking;
    return result;
  }

  ContentBlock._();

  factory ContentBlock.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ContentBlock.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static const $core.Map<$core.int, ContentBlock_Block>
      _ContentBlock_BlockByTag = {
    1: ContentBlock_Block.text,
    2: ContentBlock_Block.image,
    3: ContentBlock_Block.thinking,
    0: ContentBlock_Block.notSet
  };
  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ContentBlock',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..oo(0, [1, 2, 3])
    ..aOM<TextBlock>(1, _omitFieldNames ? '' : 'text',
        subBuilder: TextBlock.create)
    ..aOM<ImageBlock>(2, _omitFieldNames ? '' : 'image',
        subBuilder: ImageBlock.create)
    ..aOM<ThinkingBlock>(3, _omitFieldNames ? '' : 'thinking',
        subBuilder: ThinkingBlock.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ContentBlock clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ContentBlock copyWith(void Function(ContentBlock) updates) =>
      super.copyWith((message) => updates(message as ContentBlock))
          as ContentBlock;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ContentBlock create() => ContentBlock._();
  @$core.override
  ContentBlock createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ContentBlock getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ContentBlock>(create);
  static ContentBlock? _defaultInstance;

  @$pb.TagNumber(1)
  @$pb.TagNumber(2)
  @$pb.TagNumber(3)
  ContentBlock_Block whichBlock() => _ContentBlock_BlockByTag[$_whichOneof(0)]!;
  @$pb.TagNumber(1)
  @$pb.TagNumber(2)
  @$pb.TagNumber(3)
  void clearBlock() => $_clearField($_whichOneof(0));

  @$pb.TagNumber(1)
  TextBlock get text => $_getN(0);
  @$pb.TagNumber(1)
  set text(TextBlock value) => $_setField(1, value);
  @$pb.TagNumber(1)
  $core.bool hasText() => $_has(0);
  @$pb.TagNumber(1)
  void clearText() => $_clearField(1);
  @$pb.TagNumber(1)
  TextBlock ensureText() => $_ensure(0);

  @$pb.TagNumber(2)
  ImageBlock get image => $_getN(1);
  @$pb.TagNumber(2)
  set image(ImageBlock value) => $_setField(2, value);
  @$pb.TagNumber(2)
  $core.bool hasImage() => $_has(1);
  @$pb.TagNumber(2)
  void clearImage() => $_clearField(2);
  @$pb.TagNumber(2)
  ImageBlock ensureImage() => $_ensure(1);

  @$pb.TagNumber(3)
  ThinkingBlock get thinking => $_getN(2);
  @$pb.TagNumber(3)
  set thinking(ThinkingBlock value) => $_setField(3, value);
  @$pb.TagNumber(3)
  $core.bool hasThinking() => $_has(2);
  @$pb.TagNumber(3)
  void clearThinking() => $_clearField(3);
  @$pb.TagNumber(3)
  ThinkingBlock ensureThinking() => $_ensure(2);
}

class TextBlock extends $pb.GeneratedMessage {
  factory TextBlock({
    $core.String? text,
  }) {
    final result = create();
    if (text != null) result.text = text;
    return result;
  }

  TextBlock._();

  factory TextBlock.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory TextBlock.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'TextBlock',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'text')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TextBlock clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TextBlock copyWith(void Function(TextBlock) updates) =>
      super.copyWith((message) => updates(message as TextBlock)) as TextBlock;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static TextBlock create() => TextBlock._();
  @$core.override
  TextBlock createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static TextBlock getDefault() =>
      _defaultInstance ??= $pb.GeneratedMessage.$_defaultFor<TextBlock>(create);
  static TextBlock? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get text => $_getSZ(0);
  @$pb.TagNumber(1)
  set text($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasText() => $_has(0);
  @$pb.TagNumber(1)
  void clearText() => $_clearField(1);
}

class ImageBlock extends $pb.GeneratedMessage {
  factory ImageBlock({
    $core.String? mediaType,
    $core.List<$core.int>? data,
  }) {
    final result = create();
    if (mediaType != null) result.mediaType = mediaType;
    if (data != null) result.data = data;
    return result;
  }

  ImageBlock._();

  factory ImageBlock.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ImageBlock.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ImageBlock',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'mediaType')
    ..a<$core.List<$core.int>>(
        2, _omitFieldNames ? '' : 'data', $pb.PbFieldType.OY)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ImageBlock clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ImageBlock copyWith(void Function(ImageBlock) updates) =>
      super.copyWith((message) => updates(message as ImageBlock)) as ImageBlock;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ImageBlock create() => ImageBlock._();
  @$core.override
  ImageBlock createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ImageBlock getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ImageBlock>(create);
  static ImageBlock? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get mediaType => $_getSZ(0);
  @$pb.TagNumber(1)
  set mediaType($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasMediaType() => $_has(0);
  @$pb.TagNumber(1)
  void clearMediaType() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.List<$core.int> get data => $_getN(1);
  @$pb.TagNumber(2)
  set data($core.List<$core.int> value) => $_setBytes(1, value);
  @$pb.TagNumber(2)
  $core.bool hasData() => $_has(1);
  @$pb.TagNumber(2)
  void clearData() => $_clearField(2);
}

class ThinkingBlock extends $pb.GeneratedMessage {
  factory ThinkingBlock({
    $core.String? text,
  }) {
    final result = create();
    if (text != null) result.text = text;
    return result;
  }

  ThinkingBlock._();

  factory ThinkingBlock.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ThinkingBlock.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ThinkingBlock',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'text')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ThinkingBlock clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ThinkingBlock copyWith(void Function(ThinkingBlock) updates) =>
      super.copyWith((message) => updates(message as ThinkingBlock))
          as ThinkingBlock;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ThinkingBlock create() => ThinkingBlock._();
  @$core.override
  ThinkingBlock createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ThinkingBlock getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ThinkingBlock>(create);
  static ThinkingBlock? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get text => $_getSZ(0);
  @$pb.TagNumber(1)
  set text($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasText() => $_has(0);
  @$pb.TagNumber(1)
  void clearText() => $_clearField(1);
}

class ToolDefinition extends $pb.GeneratedMessage {
  factory ToolDefinition({
    $core.String? name,
    $core.String? description,
    $core.String? parametersJson,
  }) {
    final result = create();
    if (name != null) result.name = name;
    if (description != null) result.description = description;
    if (parametersJson != null) result.parametersJson = parametersJson;
    return result;
  }

  ToolDefinition._();

  factory ToolDefinition.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ToolDefinition.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ToolDefinition',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'name')
    ..aOS(2, _omitFieldNames ? '' : 'description')
    ..aOS(3, _omitFieldNames ? '' : 'parametersJson')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolDefinition clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolDefinition copyWith(void Function(ToolDefinition) updates) =>
      super.copyWith((message) => updates(message as ToolDefinition))
          as ToolDefinition;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ToolDefinition create() => ToolDefinition._();
  @$core.override
  ToolDefinition createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ToolDefinition getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ToolDefinition>(create);
  static ToolDefinition? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get name => $_getSZ(0);
  @$pb.TagNumber(1)
  set name($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasName() => $_has(0);
  @$pb.TagNumber(1)
  void clearName() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get description => $_getSZ(1);
  @$pb.TagNumber(2)
  set description($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasDescription() => $_has(1);
  @$pb.TagNumber(2)
  void clearDescription() => $_clearField(2);

  @$pb.TagNumber(3)
  $core.String get parametersJson => $_getSZ(2);
  @$pb.TagNumber(3)
  set parametersJson($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasParametersJson() => $_has(2);
  @$pb.TagNumber(3)
  void clearParametersJson() => $_clearField(3);
}

class ToolCall extends $pb.GeneratedMessage {
  factory ToolCall({
    $core.String? id,
    $core.String? name,
    $core.String? inputJson,
  }) {
    final result = create();
    if (id != null) result.id = id;
    if (name != null) result.name = name;
    if (inputJson != null) result.inputJson = inputJson;
    return result;
  }

  ToolCall._();

  factory ToolCall.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ToolCall.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ToolCall',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'id')
    ..aOS(2, _omitFieldNames ? '' : 'name')
    ..aOS(3, _omitFieldNames ? '' : 'inputJson')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolCall clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolCall copyWith(void Function(ToolCall) updates) =>
      super.copyWith((message) => updates(message as ToolCall)) as ToolCall;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ToolCall create() => ToolCall._();
  @$core.override
  ToolCall createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ToolCall getDefault() =>
      _defaultInstance ??= $pb.GeneratedMessage.$_defaultFor<ToolCall>(create);
  static ToolCall? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get id => $_getSZ(0);
  @$pb.TagNumber(1)
  set id($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasId() => $_has(0);
  @$pb.TagNumber(1)
  void clearId() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get name => $_getSZ(1);
  @$pb.TagNumber(2)
  set name($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasName() => $_has(1);
  @$pb.TagNumber(2)
  void clearName() => $_clearField(2);

  @$pb.TagNumber(3)
  $core.String get inputJson => $_getSZ(2);
  @$pb.TagNumber(3)
  set inputJson($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasInputJson() => $_has(2);
  @$pb.TagNumber(3)
  void clearInputJson() => $_clearField(3);
}

class Message extends $pb.GeneratedMessage {
  factory Message({
    $core.String? role,
    $core.Iterable<ContentBlock>? content,
    $core.Iterable<ToolCall>? toolCalls,
    $core.String? toolCallId,
    $core.bool? isError,
  }) {
    final result = create();
    if (role != null) result.role = role;
    if (content != null) result.content.addAll(content);
    if (toolCalls != null) result.toolCalls.addAll(toolCalls);
    if (toolCallId != null) result.toolCallId = toolCallId;
    if (isError != null) result.isError = isError;
    return result;
  }

  Message._();

  factory Message.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory Message.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'Message',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'role')
    ..pPM<ContentBlock>(2, _omitFieldNames ? '' : 'content',
        subBuilder: ContentBlock.create)
    ..pPM<ToolCall>(3, _omitFieldNames ? '' : 'toolCalls',
        subBuilder: ToolCall.create)
    ..aOS(4, _omitFieldNames ? '' : 'toolCallId')
    ..aOB(5, _omitFieldNames ? '' : 'isError')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  Message clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  Message copyWith(void Function(Message) updates) =>
      super.copyWith((message) => updates(message as Message)) as Message;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static Message create() => Message._();
  @$core.override
  Message createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static Message getDefault() =>
      _defaultInstance ??= $pb.GeneratedMessage.$_defaultFor<Message>(create);
  static Message? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get role => $_getSZ(0);
  @$pb.TagNumber(1)
  set role($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasRole() => $_has(0);
  @$pb.TagNumber(1)
  void clearRole() => $_clearField(1);

  @$pb.TagNumber(2)
  $pb.PbList<ContentBlock> get content => $_getList(1);

  @$pb.TagNumber(3)
  $pb.PbList<ToolCall> get toolCalls => $_getList(2);

  @$pb.TagNumber(4)
  $core.String get toolCallId => $_getSZ(3);
  @$pb.TagNumber(4)
  set toolCallId($core.String value) => $_setString(3, value);
  @$pb.TagNumber(4)
  $core.bool hasToolCallId() => $_has(3);
  @$pb.TagNumber(4)
  void clearToolCallId() => $_clearField(4);

  @$pb.TagNumber(5)
  $core.bool get isError => $_getBF(4);
  @$pb.TagNumber(5)
  set isError($core.bool value) => $_setBool(4, value);
  @$pb.TagNumber(5)
  $core.bool hasIsError() => $_has(4);
  @$pb.TagNumber(5)
  void clearIsError() => $_clearField(5);
}

class MintConversationRequest extends $pb.GeneratedMessage {
  factory MintConversationRequest() => create();

  MintConversationRequest._();

  factory MintConversationRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory MintConversationRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'MintConversationRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  MintConversationRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  MintConversationRequest copyWith(
          void Function(MintConversationRequest) updates) =>
      super.copyWith((message) => updates(message as MintConversationRequest))
          as MintConversationRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static MintConversationRequest create() => MintConversationRequest._();
  @$core.override
  MintConversationRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static MintConversationRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<MintConversationRequest>(create);
  static MintConversationRequest? _defaultInstance;
}

class MintConversationResponse extends $pb.GeneratedMessage {
  factory MintConversationResponse({
    $core.String? conversationId,
  }) {
    final result = create();
    if (conversationId != null) result.conversationId = conversationId;
    return result;
  }

  MintConversationResponse._();

  factory MintConversationResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory MintConversationResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'MintConversationResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'conversationId')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  MintConversationResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  MintConversationResponse copyWith(
          void Function(MintConversationResponse) updates) =>
      super.copyWith((message) => updates(message as MintConversationResponse))
          as MintConversationResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static MintConversationResponse create() => MintConversationResponse._();
  @$core.override
  MintConversationResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static MintConversationResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<MintConversationResponse>(create);
  static MintConversationResponse? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get conversationId => $_getSZ(0);
  @$pb.TagNumber(1)
  set conversationId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasConversationId() => $_has(0);
  @$pb.TagNumber(1)
  void clearConversationId() => $_clearField(1);
}

class ListConversationsRequest extends $pb.GeneratedMessage {
  factory ListConversationsRequest({
    $core.String? workspace,
  }) {
    final result = create();
    if (workspace != null) result.workspace = workspace;
    return result;
  }

  ListConversationsRequest._();

  factory ListConversationsRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ListConversationsRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ListConversationsRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'workspace')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListConversationsRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListConversationsRequest copyWith(
          void Function(ListConversationsRequest) updates) =>
      super.copyWith((message) => updates(message as ListConversationsRequest))
          as ListConversationsRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ListConversationsRequest create() => ListConversationsRequest._();
  @$core.override
  ListConversationsRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ListConversationsRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ListConversationsRequest>(create);
  static ListConversationsRequest? _defaultInstance;

  /// Workspace whose conversations should be listed. Authentication's
  /// workspace claim is what's actually checked by the verifier, so this
  /// field is for clarity / future cross-workspace patterns.
  @$pb.TagNumber(1)
  $core.String get workspace => $_getSZ(0);
  @$pb.TagNumber(1)
  set workspace($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasWorkspace() => $_has(0);
  @$pb.TagNumber(1)
  void clearWorkspace() => $_clearField(1);
}

class ListConversationsResponse extends $pb.GeneratedMessage {
  factory ListConversationsResponse({
    $core.Iterable<ConversationSummary>? conversations,
  }) {
    final result = create();
    if (conversations != null) result.conversations.addAll(conversations);
    return result;
  }

  ListConversationsResponse._();

  factory ListConversationsResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ListConversationsResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ListConversationsResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..pPM<ConversationSummary>(2, _omitFieldNames ? '' : 'conversations',
        subBuilder: ConversationSummary.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListConversationsResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListConversationsResponse copyWith(
          void Function(ListConversationsResponse) updates) =>
      super.copyWith((message) => updates(message as ListConversationsResponse))
          as ListConversationsResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ListConversationsResponse create() => ListConversationsResponse._();
  @$core.override
  ListConversationsResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ListConversationsResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ListConversationsResponse>(create);
  static ListConversationsResponse? _defaultInstance;

  /// Unordered. Clients render their own ordering (alpha, locally tracked
  /// MRU, etc.); server does not impose one.
  @$pb.TagNumber(2)
  $pb.PbList<ConversationSummary> get conversations => $_getList(0);
}

class ConversationSummary extends $pb.GeneratedMessage {
  factory ConversationSummary({
    $core.String? conversationId,
    $fixnum.Int64? lastTouchedMsEpoch,
    $core.String? name,
  }) {
    final result = create();
    if (conversationId != null) result.conversationId = conversationId;
    if (lastTouchedMsEpoch != null)
      result.lastTouchedMsEpoch = lastTouchedMsEpoch;
    if (name != null) result.name = name;
    return result;
  }

  ConversationSummary._();

  factory ConversationSummary.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ConversationSummary.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ConversationSummary',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'conversationId')
    ..aInt64(2, _omitFieldNames ? '' : 'lastTouchedMsEpoch')
    ..aOS(3, _omitFieldNames ? '' : 'name')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ConversationSummary clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ConversationSummary copyWith(void Function(ConversationSummary) updates) =>
      super.copyWith((message) => updates(message as ConversationSummary))
          as ConversationSummary;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ConversationSummary create() => ConversationSummary._();
  @$core.override
  ConversationSummary createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ConversationSummary getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ConversationSummary>(create);
  static ConversationSummary? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get conversationId => $_getSZ(0);
  @$pb.TagNumber(1)
  set conversationId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasConversationId() => $_has(0);
  @$pb.TagNumber(1)
  void clearConversationId() => $_clearField(1);

  /// Unix epoch milliseconds. 0 = never touched (newly minted, no
  /// messages yet). Not persisted across controller restart.
  @$pb.TagNumber(2)
  $fixnum.Int64 get lastTouchedMsEpoch => $_getI64(1);
  @$pb.TagNumber(2)
  set lastTouchedMsEpoch($fixnum.Int64 value) => $_setInt64(1, value);
  @$pb.TagNumber(2)
  $core.bool hasLastTouchedMsEpoch() => $_has(1);
  @$pb.TagNumber(2)
  void clearLastTouchedMsEpoch() => $_clearField(2);

  /// User-facing name. Defaults to a short id-derived stub at mint; user
  /// may rename via SetConversationName. Persisted on the controller PVC.
  @$pb.TagNumber(3)
  $core.String get name => $_getSZ(2);
  @$pb.TagNumber(3)
  set name($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasName() => $_has(2);
  @$pb.TagNumber(3)
  void clearName() => $_clearField(3);
}

class DeleteConversationRequest extends $pb.GeneratedMessage {
  factory DeleteConversationRequest({
    $core.String? conversationId,
  }) {
    final result = create();
    if (conversationId != null) result.conversationId = conversationId;
    return result;
  }

  DeleteConversationRequest._();

  factory DeleteConversationRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory DeleteConversationRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'DeleteConversationRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'conversationId')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeleteConversationRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeleteConversationRequest copyWith(
          void Function(DeleteConversationRequest) updates) =>
      super.copyWith((message) => updates(message as DeleteConversationRequest))
          as DeleteConversationRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static DeleteConversationRequest create() => DeleteConversationRequest._();
  @$core.override
  DeleteConversationRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static DeleteConversationRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<DeleteConversationRequest>(create);
  static DeleteConversationRequest? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get conversationId => $_getSZ(0);
  @$pb.TagNumber(1)
  set conversationId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasConversationId() => $_has(0);
  @$pb.TagNumber(1)
  void clearConversationId() => $_clearField(1);
}

class DeleteConversationResponse extends $pb.GeneratedMessage {
  factory DeleteConversationResponse() => create();

  DeleteConversationResponse._();

  factory DeleteConversationResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory DeleteConversationResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'DeleteConversationResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeleteConversationResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  DeleteConversationResponse copyWith(
          void Function(DeleteConversationResponse) updates) =>
      super.copyWith(
              (message) => updates(message as DeleteConversationResponse))
          as DeleteConversationResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static DeleteConversationResponse create() => DeleteConversationResponse._();
  @$core.override
  DeleteConversationResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static DeleteConversationResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<DeleteConversationResponse>(create);
  static DeleteConversationResponse? _defaultInstance;
}

class SetConversationNameRequest extends $pb.GeneratedMessage {
  factory SetConversationNameRequest({
    $core.String? conversationId,
    $core.String? name,
  }) {
    final result = create();
    if (conversationId != null) result.conversationId = conversationId;
    if (name != null) result.name = name;
    return result;
  }

  SetConversationNameRequest._();

  factory SetConversationNameRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory SetConversationNameRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'SetConversationNameRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'conversationId')
    ..aOS(2, _omitFieldNames ? '' : 'name')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SetConversationNameRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SetConversationNameRequest copyWith(
          void Function(SetConversationNameRequest) updates) =>
      super.copyWith(
              (message) => updates(message as SetConversationNameRequest))
          as SetConversationNameRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static SetConversationNameRequest create() => SetConversationNameRequest._();
  @$core.override
  SetConversationNameRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static SetConversationNameRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<SetConversationNameRequest>(create);
  static SetConversationNameRequest? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get conversationId => $_getSZ(0);
  @$pb.TagNumber(1)
  set conversationId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasConversationId() => $_has(0);
  @$pb.TagNumber(1)
  void clearConversationId() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get name => $_getSZ(1);
  @$pb.TagNumber(2)
  set name($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasName() => $_has(1);
  @$pb.TagNumber(2)
  void clearName() => $_clearField(2);
}

class SetConversationNameResponse extends $pb.GeneratedMessage {
  factory SetConversationNameResponse() => create();

  SetConversationNameResponse._();

  factory SetConversationNameResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory SetConversationNameResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'SetConversationNameResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SetConversationNameResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SetConversationNameResponse copyWith(
          void Function(SetConversationNameResponse) updates) =>
      super.copyWith(
              (message) => updates(message as SetConversationNameResponse))
          as SetConversationNameResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static SetConversationNameResponse create() =>
      SetConversationNameResponse._();
  @$core.override
  SetConversationNameResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static SetConversationNameResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<SetConversationNameResponse>(create);
  static SetConversationNameResponse? _defaultInstance;
}

class ListWorkspacesRequest extends $pb.GeneratedMessage {
  factory ListWorkspacesRequest() => create();

  ListWorkspacesRequest._();

  factory ListWorkspacesRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ListWorkspacesRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ListWorkspacesRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListWorkspacesRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListWorkspacesRequest copyWith(
          void Function(ListWorkspacesRequest) updates) =>
      super.copyWith((message) => updates(message as ListWorkspacesRequest))
          as ListWorkspacesRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ListWorkspacesRequest create() => ListWorkspacesRequest._();
  @$core.override
  ListWorkspacesRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ListWorkspacesRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ListWorkspacesRequest>(create);
  static ListWorkspacesRequest? _defaultInstance;
}

class ListWorkspacesResponse extends $pb.GeneratedMessage {
  factory ListWorkspacesResponse({
    $core.Iterable<$core.String>? workspaces,
  }) {
    final result = create();
    if (workspaces != null) result.workspaces.addAll(workspaces);
    return result;
  }

  ListWorkspacesResponse._();

  factory ListWorkspacesResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ListWorkspacesResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ListWorkspacesResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..pPS(1, _omitFieldNames ? '' : 'workspaces')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListWorkspacesResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListWorkspacesResponse copyWith(
          void Function(ListWorkspacesResponse) updates) =>
      super.copyWith((message) => updates(message as ListWorkspacesResponse))
          as ListWorkspacesResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ListWorkspacesResponse create() => ListWorkspacesResponse._();
  @$core.override
  ListWorkspacesResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ListWorkspacesResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ListWorkspacesResponse>(create);
  static ListWorkspacesResponse? _defaultInstance;

  @$pb.TagNumber(1)
  $pb.PbList<$core.String> get workspaces => $_getList(0);
}

class ChannelAck extends $pb.GeneratedMessage {
  factory ChannelAck({
    $core.String? channelId,
  }) {
    final result = create();
    if (channelId != null) result.channelId = channelId;
    return result;
  }

  ChannelAck._();

  factory ChannelAck.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ChannelAck.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ChannelAck',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'channelId')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelAck clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelAck copyWith(void Function(ChannelAck) updates) =>
      super.copyWith((message) => updates(message as ChannelAck)) as ChannelAck;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ChannelAck create() => ChannelAck._();
  @$core.override
  ChannelAck createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ChannelAck getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ChannelAck>(create);
  static ChannelAck? _defaultInstance;

  /// Server-minted opaque UUID. Echo on every ChannelIngest; valid only
  /// within the lifetime of the originating ChannelReceive /
  /// ChannelStream response stream.
  @$pb.TagNumber(1)
  $core.String get channelId => $_getSZ(0);
  @$pb.TagNumber(1)
  set channelId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasChannelId() => $_has(0);
  @$pb.TagNumber(1)
  void clearChannelId() => $_clearField(1);
}

enum ChannelOutbound_Command {
  ack,
  sendMessage,
  turnState,
  serverRequest,
  notSet
}

class ChannelOutbound extends $pb.GeneratedMessage {
  factory ChannelOutbound({
    ChannelAck? ack,
    ChannelSend? sendMessage,
    TurnStateEvent? turnState,
    ServerRequest? serverRequest,
  }) {
    final result = create();
    if (ack != null) result.ack = ack;
    if (sendMessage != null) result.sendMessage = sendMessage;
    if (turnState != null) result.turnState = turnState;
    if (serverRequest != null) result.serverRequest = serverRequest;
    return result;
  }

  ChannelOutbound._();

  factory ChannelOutbound.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ChannelOutbound.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static const $core.Map<$core.int, ChannelOutbound_Command>
      _ChannelOutbound_CommandByTag = {
    1: ChannelOutbound_Command.ack,
    2: ChannelOutbound_Command.sendMessage,
    3: ChannelOutbound_Command.turnState,
    4: ChannelOutbound_Command.serverRequest,
    0: ChannelOutbound_Command.notSet
  };
  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ChannelOutbound',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..oo(0, [1, 2, 3, 4])
    ..aOM<ChannelAck>(1, _omitFieldNames ? '' : 'ack',
        subBuilder: ChannelAck.create)
    ..aOM<ChannelSend>(2, _omitFieldNames ? '' : 'sendMessage',
        subBuilder: ChannelSend.create)
    ..aOM<TurnStateEvent>(3, _omitFieldNames ? '' : 'turnState',
        subBuilder: TurnStateEvent.create)
    ..aOM<ServerRequest>(4, _omitFieldNames ? '' : 'serverRequest',
        subBuilder: ServerRequest.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelOutbound clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelOutbound copyWith(void Function(ChannelOutbound) updates) =>
      super.copyWith((message) => updates(message as ChannelOutbound))
          as ChannelOutbound;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ChannelOutbound create() => ChannelOutbound._();
  @$core.override
  ChannelOutbound createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ChannelOutbound getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ChannelOutbound>(create);
  static ChannelOutbound? _defaultInstance;

  @$pb.TagNumber(1)
  @$pb.TagNumber(2)
  @$pb.TagNumber(3)
  @$pb.TagNumber(4)
  ChannelOutbound_Command whichCommand() =>
      _ChannelOutbound_CommandByTag[$_whichOneof(0)]!;
  @$pb.TagNumber(1)
  @$pb.TagNumber(2)
  @$pb.TagNumber(3)
  @$pb.TagNumber(4)
  void clearCommand() => $_clearField($_whichOneof(0));

  @$pb.TagNumber(1)
  ChannelAck get ack => $_getN(0);
  @$pb.TagNumber(1)
  set ack(ChannelAck value) => $_setField(1, value);
  @$pb.TagNumber(1)
  $core.bool hasAck() => $_has(0);
  @$pb.TagNumber(1)
  void clearAck() => $_clearField(1);
  @$pb.TagNumber(1)
  ChannelAck ensureAck() => $_ensure(0);

  @$pb.TagNumber(2)
  ChannelSend get sendMessage => $_getN(1);
  @$pb.TagNumber(2)
  set sendMessage(ChannelSend value) => $_setField(2, value);
  @$pb.TagNumber(2)
  $core.bool hasSendMessage() => $_has(1);
  @$pb.TagNumber(2)
  void clearSendMessage() => $_clearField(2);
  @$pb.TagNumber(2)
  ChannelSend ensureSendMessage() => $_ensure(1);

  @$pb.TagNumber(3)
  TurnStateEvent get turnState => $_getN(2);
  @$pb.TagNumber(3)
  set turnState(TurnStateEvent value) => $_setField(3, value);
  @$pb.TagNumber(3)
  $core.bool hasTurnState() => $_has(2);
  @$pb.TagNumber(3)
  void clearTurnState() => $_clearField(3);
  @$pb.TagNumber(3)
  TurnStateEvent ensureTurnState() => $_ensure(2);

  @$pb.TagNumber(4)
  ServerRequest get serverRequest => $_getN(3);
  @$pb.TagNumber(4)
  set serverRequest(ServerRequest value) => $_setField(4, value);
  @$pb.TagNumber(4)
  $core.bool hasServerRequest() => $_has(3);
  @$pb.TagNumber(4)
  void clearServerRequest() => $_clearField(4);
  @$pb.TagNumber(4)
  ServerRequest ensureServerRequest() => $_ensure(3);
}

class ChannelSend extends $pb.GeneratedMessage {
  factory ChannelSend({
    $core.Iterable<ContentBlock>? content,
    $core.String? conversationId,
  }) {
    final result = create();
    if (content != null) result.content.addAll(content);
    if (conversationId != null) result.conversationId = conversationId;
    return result;
  }

  ChannelSend._();

  factory ChannelSend.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ChannelSend.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ChannelSend',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..pPM<ContentBlock>(1, _omitFieldNames ? '' : 'content',
        subBuilder: ContentBlock.create)
    ..aOS(2, _omitFieldNames ? '' : 'conversationId')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelSend clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelSend copyWith(void Function(ChannelSend) updates) =>
      super.copyWith((message) => updates(message as ChannelSend))
          as ChannelSend;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ChannelSend create() => ChannelSend._();
  @$core.override
  ChannelSend createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ChannelSend getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ChannelSend>(create);
  static ChannelSend? _defaultInstance;

  @$pb.TagNumber(1)
  $pb.PbList<ContentBlock> get content => $_getList(0);

  /// Conversation this reply belongs to. Lets the client filter outbound
  /// frames against whichever conversation it is currently viewing.
  @$pb.TagNumber(2)
  $core.String get conversationId => $_getSZ(1);
  @$pb.TagNumber(2)
  set conversationId($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasConversationId() => $_has(1);
  @$pb.TagNumber(2)
  void clearConversationId() => $_clearField(2);
}

/// Streaming turn-lifecycle phase. The controller pushes this to every
/// ChannelReceive subscriber so clients can render a "working" indicator
/// without polling or per-tool detail. Sending (between user submit and
/// the controller's ChannelIngest ack) stays purely client-side; only
/// WORKING and IDLE are server-pushed today.
class TurnStateEvent extends $pb.GeneratedMessage {
  factory TurnStateEvent({
    TurnState? state,
    $core.String? conversationId,
    $core.String? reason,
    $core.String? code,
  }) {
    final result = create();
    if (state != null) result.state = state;
    if (conversationId != null) result.conversationId = conversationId;
    if (reason != null) result.reason = reason;
    if (code != null) result.code = code;
    return result;
  }

  TurnStateEvent._();

  factory TurnStateEvent.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory TurnStateEvent.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'TurnStateEvent',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aE<TurnState>(1, _omitFieldNames ? '' : 'state',
        enumValues: TurnState.values)
    ..aOS(2, _omitFieldNames ? '' : 'conversationId')
    ..aOS(3, _omitFieldNames ? '' : 'reason')
    ..aOS(4, _omitFieldNames ? '' : 'code')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnStateEvent clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnStateEvent copyWith(void Function(TurnStateEvent) updates) =>
      super.copyWith((message) => updates(message as TurnStateEvent))
          as TurnStateEvent;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static TurnStateEvent create() => TurnStateEvent._();
  @$core.override
  TurnStateEvent createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static TurnStateEvent getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<TurnStateEvent>(create);
  static TurnStateEvent? _defaultInstance;

  @$pb.TagNumber(1)
  TurnState get state => $_getN(0);
  @$pb.TagNumber(1)
  set state(TurnState value) => $_setField(1, value);
  @$pb.TagNumber(1)
  $core.bool hasState() => $_has(0);
  @$pb.TagNumber(1)
  void clearState() => $_clearField(1);

  /// Conversation this state transition belongs to. Empty string is
  /// reserved for channel-wide replay frames emitted on (re)connect —
  /// clients should treat empty as "no conversation context" and skip
  /// the per-conversation indicator update.
  @$pb.TagNumber(2)
  $core.String get conversationId => $_getSZ(1);
  @$pb.TagNumber(2)
  set conversationId($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasConversationId() => $_has(1);
  @$pb.TagNumber(2)
  void clearConversationId() => $_clearField(2);

  /// Human-readable failure reason; set only when state == FAILED.
  @$pb.TagNumber(3)
  $core.String get reason => $_getSZ(2);
  @$pb.TagNumber(3)
  set reason($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasReason() => $_has(2);
  @$pb.TagNumber(3)
  void clearReason() => $_clearField(3);

  /// Machine-readable failure code (e.g. gRPC status code as a string);
  /// set only when state == FAILED.
  @$pb.TagNumber(4)
  $core.String get code => $_getSZ(3);
  @$pb.TagNumber(4)
  set code($core.String value) => $_setString(3, value);
  @$pb.TagNumber(4)
  $core.bool hasCode() => $_has(3);
  @$pb.TagNumber(4)
  void clearCode() => $_clearField(4);
}

/// UserMessage is the canonical "user said something" event. Flows from
/// Channel Job → Hangar → Transponder. `reply_channel` is the
/// server-minted channel_id (UUID) stamped by Hangar after
/// ChannelIngest / ChannelStream message ingress, so the transponder's
/// reply routes back to the originating adapter. Opaque to clients;
/// only valid within the lifetime of the originating ChannelReceive /
/// ChannelStream stream.
class UserMessage extends $pb.GeneratedMessage {
  factory UserMessage({
    $core.Iterable<ContentBlock>? content,
    $core.String? sender,
    $core.String? replyChannel,
    $core.String? conversationId,
  }) {
    final result = create();
    if (content != null) result.content.addAll(content);
    if (sender != null) result.sender = sender;
    if (replyChannel != null) result.replyChannel = replyChannel;
    if (conversationId != null) result.conversationId = conversationId;
    return result;
  }

  UserMessage._();

  factory UserMessage.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory UserMessage.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'UserMessage',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..pPM<ContentBlock>(1, _omitFieldNames ? '' : 'content',
        subBuilder: ContentBlock.create)
    ..aOS(2, _omitFieldNames ? '' : 'sender')
    ..aOS(3, _omitFieldNames ? '' : 'replyChannel')
    ..aOS(4, _omitFieldNames ? '' : 'conversationId')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  UserMessage clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  UserMessage copyWith(void Function(UserMessage) updates) =>
      super.copyWith((message) => updates(message as UserMessage))
          as UserMessage;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static UserMessage create() => UserMessage._();
  @$core.override
  UserMessage createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static UserMessage getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<UserMessage>(create);
  static UserMessage? _defaultInstance;

  @$pb.TagNumber(1)
  $pb.PbList<ContentBlock> get content => $_getList(0);

  @$pb.TagNumber(2)
  $core.String get sender => $_getSZ(1);
  @$pb.TagNumber(2)
  set sender($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasSender() => $_has(1);
  @$pb.TagNumber(2)
  void clearSender() => $_clearField(2);

  @$pb.TagNumber(3)
  $core.String get replyChannel => $_getSZ(2);
  @$pb.TagNumber(3)
  set replyChannel($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasReplyChannel() => $_has(2);
  @$pb.TagNumber(3)
  void clearReplyChannel() => $_clearField(3);

  /// Conversation this message belongs to. Stamped by hangar at
  /// ingest time (caller may supply via ChannelIngestRequest, or the
  /// controller mints a fresh id). The transponder reads this verbatim
  /// when constructing the TurnRequest — it never mints conversation
  /// ids on its own anymore.
  @$pb.TagNumber(4)
  $core.String get conversationId => $_getSZ(3);
  @$pb.TagNumber(4)
  set conversationId($core.String value) => $_setString(3, value);
  @$pb.TagNumber(4)
  $core.bool hasConversationId() => $_has(3);
  @$pb.TagNumber(4)
  void clearConversationId() => $_clearField(4);
}

class GetConversationHistoryRequest extends $pb.GeneratedMessage {
  factory GetConversationHistoryRequest({
    $core.String? conversationId,
    $core.int? limit,
  }) {
    final result = create();
    if (conversationId != null) result.conversationId = conversationId;
    if (limit != null) result.limit = limit;
    return result;
  }

  GetConversationHistoryRequest._();

  factory GetConversationHistoryRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory GetConversationHistoryRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'GetConversationHistoryRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'conversationId')
    ..aI(2, _omitFieldNames ? '' : 'limit', fieldType: $pb.PbFieldType.OU3)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  GetConversationHistoryRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  GetConversationHistoryRequest copyWith(
          void Function(GetConversationHistoryRequest) updates) =>
      super.copyWith(
              (message) => updates(message as GetConversationHistoryRequest))
          as GetConversationHistoryRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static GetConversationHistoryRequest create() =>
      GetConversationHistoryRequest._();
  @$core.override
  GetConversationHistoryRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static GetConversationHistoryRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<GetConversationHistoryRequest>(create);
  static GetConversationHistoryRequest? _defaultInstance;

  /// Conversation to read; must belong to the workspace the calling SA
  /// token authorizes for.
  @$pb.TagNumber(1)
  $core.String get conversationId => $_getSZ(0);
  @$pb.TagNumber(1)
  set conversationId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasConversationId() => $_has(0);
  @$pb.TagNumber(1)
  void clearConversationId() => $_clearField(1);

  /// Max number of recent entries to return. Server clamps to a sane
  /// ceiling; 0 / unset → server default (~50).
  @$pb.TagNumber(2)
  $core.int get limit => $_getIZ(1);
  @$pb.TagNumber(2)
  set limit($core.int value) => $_setUnsignedInt32(1, value);
  @$pb.TagNumber(2)
  $core.bool hasLimit() => $_has(1);
  @$pb.TagNumber(2)
  void clearLimit() => $_clearField(2);
}

class GetConversationHistoryResponse extends $pb.GeneratedMessage {
  factory GetConversationHistoryResponse({
    $core.Iterable<HistoryEntry>? entries,
    $fixnum.Int64? totalSeq,
    $core.bool? truncated,
  }) {
    final result = create();
    if (entries != null) result.entries.addAll(entries);
    if (totalSeq != null) result.totalSeq = totalSeq;
    if (truncated != null) result.truncated = truncated;
    return result;
  }

  GetConversationHistoryResponse._();

  factory GetConversationHistoryResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory GetConversationHistoryResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'GetConversationHistoryResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..pPM<HistoryEntry>(1, _omitFieldNames ? '' : 'entries',
        subBuilder: HistoryEntry.create)
    ..a<$fixnum.Int64>(
        2, _omitFieldNames ? '' : 'totalSeq', $pb.PbFieldType.OU6,
        defaultOrMaker: $fixnum.Int64.ZERO)
    ..aOB(3, _omitFieldNames ? '' : 'truncated')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  GetConversationHistoryResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  GetConversationHistoryResponse copyWith(
          void Function(GetConversationHistoryResponse) updates) =>
      super.copyWith(
              (message) => updates(message as GetConversationHistoryResponse))
          as GetConversationHistoryResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static GetConversationHistoryResponse create() =>
      GetConversationHistoryResponse._();
  @$core.override
  GetConversationHistoryResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static GetConversationHistoryResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<GetConversationHistoryResponse>(create);
  static GetConversationHistoryResponse? _defaultInstance;

  /// Recent entries in oldest-to-newest order (tail of the log).
  @$pb.TagNumber(1)
  $pb.PbList<HistoryEntry> get entries => $_getList(0);

  /// Total log length at snapshot time. Lets the caller see if it
  /// received a truncated view.
  @$pb.TagNumber(2)
  $fixnum.Int64 get totalSeq => $_getI64(1);
  @$pb.TagNumber(2)
  set totalSeq($fixnum.Int64 value) => $_setInt64(1, value);
  @$pb.TagNumber(2)
  $core.bool hasTotalSeq() => $_has(1);
  @$pb.TagNumber(2)
  void clearTotalSeq() => $_clearField(2);

  /// True when `limit` clipped the head of the log.
  @$pb.TagNumber(3)
  $core.bool get truncated => $_getBF(2);
  @$pb.TagNumber(3)
  set truncated($core.bool value) => $_setBool(2, value);
  @$pb.TagNumber(3)
  $core.bool hasTruncated() => $_has(2);
  @$pb.TagNumber(3)
  void clearTruncated() => $_clearField(3);
}

class HistoryEntry extends $pb.GeneratedMessage {
  factory HistoryEntry({
    $fixnum.Int64? seq,
    $core.String? ts,
    Message? message,
    $core.String? tag,
  }) {
    final result = create();
    if (seq != null) result.seq = seq;
    if (ts != null) result.ts = ts;
    if (message != null) result.message = message;
    if (tag != null) result.tag = tag;
    return result;
  }

  HistoryEntry._();

  factory HistoryEntry.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory HistoryEntry.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'HistoryEntry',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..a<$fixnum.Int64>(1, _omitFieldNames ? '' : 'seq', $pb.PbFieldType.OU6,
        defaultOrMaker: $fixnum.Int64.ZERO)
    ..aOS(2, _omitFieldNames ? '' : 'ts')
    ..aOM<Message>(3, _omitFieldNames ? '' : 'message',
        subBuilder: Message.create)
    ..aOS(4, _omitFieldNames ? '' : 'tag')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  HistoryEntry clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  HistoryEntry copyWith(void Function(HistoryEntry) updates) =>
      super.copyWith((message) => updates(message as HistoryEntry))
          as HistoryEntry;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static HistoryEntry create() => HistoryEntry._();
  @$core.override
  HistoryEntry createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static HistoryEntry getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<HistoryEntry>(create);
  static HistoryEntry? _defaultInstance;

  /// 1-indexed sequence in the conversation log.
  @$pb.TagNumber(1)
  $fixnum.Int64 get seq => $_getI64(0);
  @$pb.TagNumber(1)
  set seq($fixnum.Int64 value) => $_setInt64(0, value);
  @$pb.TagNumber(1)
  $core.bool hasSeq() => $_has(0);
  @$pb.TagNumber(1)
  void clearSeq() => $_clearField(1);

  /// RFC 3339 timestamp from when the entry was appended.
  @$pb.TagNumber(2)
  $core.String get ts => $_getSZ(1);
  @$pb.TagNumber(2)
  set ts($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasTs() => $_has(1);
  @$pb.TagNumber(2)
  void clearTs() => $_clearField(2);

  /// The persisted message — role, content blocks, tool calls, etc.
  @$pb.TagNumber(3)
  Message get message => $_getN(2);
  @$pb.TagNumber(3)
  set message(Message value) => $_setField(3, value);
  @$pb.TagNumber(3)
  $core.bool hasMessage() => $_has(2);
  @$pb.TagNumber(3)
  void clearMessage() => $_clearField(3);
  @$pb.TagNumber(3)
  Message ensureMessage() => $_ensure(2);

  /// Scope tag if present (e.g., "delegate:<correlation_id>").
  @$pb.TagNumber(4)
  $core.String get tag => $_getSZ(3);
  @$pb.TagNumber(4)
  set tag($core.String value) => $_setString(3, value);
  @$pb.TagNumber(4)
  $core.bool hasTag() => $_has(3);
  @$pb.TagNumber(4)
  void clearTag() => $_clearField(4);
}

class GetTurnStateRequest extends $pb.GeneratedMessage {
  factory GetTurnStateRequest({
    $core.String? conversationId,
  }) {
    final result = create();
    if (conversationId != null) result.conversationId = conversationId;
    return result;
  }

  GetTurnStateRequest._();

  factory GetTurnStateRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory GetTurnStateRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'GetTurnStateRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'conversationId')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  GetTurnStateRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  GetTurnStateRequest copyWith(void Function(GetTurnStateRequest) updates) =>
      super.copyWith((message) => updates(message as GetTurnStateRequest))
          as GetTurnStateRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static GetTurnStateRequest create() => GetTurnStateRequest._();
  @$core.override
  GetTurnStateRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static GetTurnStateRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<GetTurnStateRequest>(create);
  static GetTurnStateRequest? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get conversationId => $_getSZ(0);
  @$pb.TagNumber(1)
  set conversationId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasConversationId() => $_has(0);
  @$pb.TagNumber(1)
  void clearConversationId() => $_clearField(1);
}

class WatchToolsRequest extends $pb.GeneratedMessage {
  factory WatchToolsRequest() => create();

  WatchToolsRequest._();

  factory WatchToolsRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory WatchToolsRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'WatchToolsRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  WatchToolsRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  WatchToolsRequest copyWith(void Function(WatchToolsRequest) updates) =>
      super.copyWith((message) => updates(message as WatchToolsRequest))
          as WatchToolsRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static WatchToolsRequest create() => WatchToolsRequest._();
  @$core.override
  WatchToolsRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static WatchToolsRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<WatchToolsRequest>(create);
  static WatchToolsRequest? _defaultInstance;
}

class ToolListUpdate extends $pb.GeneratedMessage {
  factory ToolListUpdate({
    $core.Iterable<ToolInfo>? tools,
  }) {
    final result = create();
    if (tools != null) result.tools.addAll(tools);
    return result;
  }

  ToolListUpdate._();

  factory ToolListUpdate.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ToolListUpdate.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ToolListUpdate',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..pPM<ToolInfo>(1, _omitFieldNames ? '' : 'tools',
        subBuilder: ToolInfo.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolListUpdate clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolListUpdate copyWith(void Function(ToolListUpdate) updates) =>
      super.copyWith((message) => updates(message as ToolListUpdate))
          as ToolListUpdate;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ToolListUpdate create() => ToolListUpdate._();
  @$core.override
  ToolListUpdate createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ToolListUpdate getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ToolListUpdate>(create);
  static ToolListUpdate? _defaultInstance;

  @$pb.TagNumber(1)
  $pb.PbList<ToolInfo> get tools => $_getList(0);
}

class ToolInfo extends $pb.GeneratedMessage {
  factory ToolInfo({
    $core.String? name,
    $core.String? description,
    $core.String? parametersJson,
  }) {
    final result = create();
    if (name != null) result.name = name;
    if (description != null) result.description = description;
    if (parametersJson != null) result.parametersJson = parametersJson;
    return result;
  }

  ToolInfo._();

  factory ToolInfo.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ToolInfo.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ToolInfo',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'name')
    ..aOS(2, _omitFieldNames ? '' : 'description')
    ..aOS(3, _omitFieldNames ? '' : 'parametersJson')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolInfo clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolInfo copyWith(void Function(ToolInfo) updates) =>
      super.copyWith((message) => updates(message as ToolInfo)) as ToolInfo;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ToolInfo create() => ToolInfo._();
  @$core.override
  ToolInfo createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ToolInfo getDefault() =>
      _defaultInstance ??= $pb.GeneratedMessage.$_defaultFor<ToolInfo>(create);
  static ToolInfo? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get name => $_getSZ(0);
  @$pb.TagNumber(1)
  set name($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasName() => $_has(0);
  @$pb.TagNumber(1)
  void clearName() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get description => $_getSZ(1);
  @$pb.TagNumber(2)
  set description($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasDescription() => $_has(1);
  @$pb.TagNumber(2)
  void clearDescription() => $_clearField(2);

  @$pb.TagNumber(3)
  $core.String get parametersJson => $_getSZ(2);
  @$pb.TagNumber(3)
  set parametersJson($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasParametersJson() => $_has(2);
  @$pb.TagNumber(3)
  void clearParametersJson() => $_clearField(3);
}

class CallToolRequest extends $pb.GeneratedMessage {
  factory CallToolRequest({
    $core.String? name,
    $core.String? inputJson,
  }) {
    final result = create();
    if (name != null) result.name = name;
    if (inputJson != null) result.inputJson = inputJson;
    return result;
  }

  CallToolRequest._();

  factory CallToolRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory CallToolRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'CallToolRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'name')
    ..aOS(2, _omitFieldNames ? '' : 'inputJson')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  CallToolRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  CallToolRequest copyWith(void Function(CallToolRequest) updates) =>
      super.copyWith((message) => updates(message as CallToolRequest))
          as CallToolRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static CallToolRequest create() => CallToolRequest._();
  @$core.override
  CallToolRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static CallToolRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<CallToolRequest>(create);
  static CallToolRequest? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get name => $_getSZ(0);
  @$pb.TagNumber(1)
  set name($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasName() => $_has(0);
  @$pb.TagNumber(1)
  void clearName() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get inputJson => $_getSZ(1);
  @$pb.TagNumber(2)
  set inputJson($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasInputJson() => $_has(1);
  @$pb.TagNumber(2)
  void clearInputJson() => $_clearField(2);
}

class CallToolResponse extends $pb.GeneratedMessage {
  factory CallToolResponse({
    $core.String? output,
    $core.bool? isError,
  }) {
    final result = create();
    if (output != null) result.output = output;
    if (isError != null) result.isError = isError;
    return result;
  }

  CallToolResponse._();

  factory CallToolResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory CallToolResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'CallToolResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'output')
    ..aOB(2, _omitFieldNames ? '' : 'isError')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  CallToolResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  CallToolResponse copyWith(void Function(CallToolResponse) updates) =>
      super.copyWith((message) => updates(message as CallToolResponse))
          as CallToolResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static CallToolResponse create() => CallToolResponse._();
  @$core.override
  CallToolResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static CallToolResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<CallToolResponse>(create);
  static CallToolResponse? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get output => $_getSZ(0);
  @$pb.TagNumber(1)
  set output($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasOutput() => $_has(0);
  @$pb.TagNumber(1)
  void clearOutput() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.bool get isError => $_getBF(1);
  @$pb.TagNumber(2)
  set isError($core.bool value) => $_setBool(1, value);
  @$pb.TagNumber(2)
  $core.bool hasIsError() => $_has(1);
  @$pb.TagNumber(2)
  void clearIsError() => $_clearField(2);
}

class ChannelIngestRequest extends $pb.GeneratedMessage {
  factory ChannelIngestRequest({
    $core.String? channelId,
    UserMessage? userMessage,
    ClientResponse? clientResponse,
    $core.Iterable<$core.String>? supportedMethods,
    $core.String? conversationId,
  }) {
    final result = create();
    if (channelId != null) result.channelId = channelId;
    if (userMessage != null) result.userMessage = userMessage;
    if (clientResponse != null) result.clientResponse = clientResponse;
    if (supportedMethods != null)
      result.supportedMethods.addAll(supportedMethods);
    if (conversationId != null) result.conversationId = conversationId;
    return result;
  }

  ChannelIngestRequest._();

  factory ChannelIngestRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ChannelIngestRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ChannelIngestRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'channelId')
    ..aOM<UserMessage>(2, _omitFieldNames ? '' : 'userMessage',
        subBuilder: UserMessage.create)
    ..aOM<ClientResponse>(3, _omitFieldNames ? '' : 'clientResponse',
        subBuilder: ClientResponse.create)
    ..pPS(4, _omitFieldNames ? '' : 'supportedMethods')
    ..aOS(5, _omitFieldNames ? '' : 'conversationId')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelIngestRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelIngestRequest copyWith(void Function(ChannelIngestRequest) updates) =>
      super.copyWith((message) => updates(message as ChannelIngestRequest))
          as ChannelIngestRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ChannelIngestRequest create() => ChannelIngestRequest._();
  @$core.override
  ChannelIngestRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ChannelIngestRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ChannelIngestRequest>(create);
  static ChannelIngestRequest? _defaultInstance;

  /// The channel_id received as the first frame on the ChannelReceive
  /// response stream. Server-minted; opaque to clients.
  @$pb.TagNumber(1)
  $core.String get channelId => $_getSZ(0);
  @$pb.TagNumber(1)
  set channelId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasChannelId() => $_has(0);
  @$pb.TagNumber(1)
  void clearChannelId() => $_clearField(1);

  /// The user's message payload. The workspace is derived from the
  /// caller's signature (NOT from this field) so external callers
  /// cannot inject into other workspaces.
  ///
  /// Exactly one of `user_message` / `client_response` MUST be set per
  /// call. Both-set or neither-set → InvalidArgument at the handler.
  @$pb.TagNumber(2)
  UserMessage get userMessage => $_getN(1);
  @$pb.TagNumber(2)
  set userMessage(UserMessage value) => $_setField(2, value);
  @$pb.TagNumber(2)
  $core.bool hasUserMessage() => $_has(1);
  @$pb.TagNumber(2)
  void clearUserMessage() => $_clearField(2);
  @$pb.TagNumber(2)
  UserMessage ensureUserMessage() => $_ensure(1);

  /// Reply to a prior ServerRequest the client received over its
  /// ChannelReceive stream. Correlation key is request_id.
  @$pb.TagNumber(3)
  ClientResponse get clientResponse => $_getN(2);
  @$pb.TagNumber(3)
  set clientResponse(ClientResponse value) => $_setField(3, value);
  @$pb.TagNumber(3)
  $core.bool hasClientResponse() => $_has(2);
  @$pb.TagNumber(3)
  void clearClientResponse() => $_clearField(3);
  @$pb.TagNumber(3)
  ClientResponse ensureClientResponse() => $_ensure(2);

  /// Methods the sending device is willing to render when the agent
  /// dispatches a ServerRequest. Last-sender-wins per channel; sent on
  /// every ingest. Names match ServerRequest.method values.
  @$pb.TagNumber(4)
  $pb.PbList<$core.String> get supportedMethods => $_getList(3);

  /// Conversation this message belongs to. Empty string → controller
  /// mints a fresh id and returns it in the ack. Non-empty → controller
  /// validates the id belongs to the caller's workspace and continues
  /// the existing conversation; mismatch returns PermissionDenied.
  @$pb.TagNumber(5)
  $core.String get conversationId => $_getSZ(4);
  @$pb.TagNumber(5)
  set conversationId($core.String value) => $_setString(4, value);
  @$pb.TagNumber(5)
  $core.bool hasConversationId() => $_has(4);
  @$pb.TagNumber(5)
  void clearConversationId() => $_clearField(5);
}

class ChannelIngestAck extends $pb.GeneratedMessage {
  factory ChannelIngestAck({
    $core.String? channelId,
    $core.String? conversationId,
  }) {
    final result = create();
    if (channelId != null) result.channelId = channelId;
    if (conversationId != null) result.conversationId = conversationId;
    return result;
  }

  ChannelIngestAck._();

  factory ChannelIngestAck.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ChannelIngestAck.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ChannelIngestAck',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'channelId')
    ..aOS(2, _omitFieldNames ? '' : 'conversationId')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelIngestAck clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelIngestAck copyWith(void Function(ChannelIngestAck) updates) =>
      super.copyWith((message) => updates(message as ChannelIngestAck))
          as ChannelIngestAck;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ChannelIngestAck create() => ChannelIngestAck._();
  @$core.override
  ChannelIngestAck createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ChannelIngestAck getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ChannelIngestAck>(create);
  static ChannelIngestAck? _defaultInstance;

  /// Echoed for caller correlation.
  @$pb.TagNumber(1)
  $core.String get channelId => $_getSZ(0);
  @$pb.TagNumber(1)
  set channelId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasChannelId() => $_has(0);
  @$pb.TagNumber(1)
  void clearChannelId() => $_clearField(1);

  /// Conversation under which this message was filed. Pair with
  /// GetConversationHistory for replay across reconnects.
  @$pb.TagNumber(2)
  $core.String get conversationId => $_getSZ(1);
  @$pb.TagNumber(2)
  set conversationId($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasConversationId() => $_has(1);
  @$pb.TagNumber(2)
  void clearConversationId() => $_clearField(2);
}

class ChannelReceiveRequest extends $pb.GeneratedMessage {
  factory ChannelReceiveRequest({
    $core.String? adapterHint,
  }) {
    final result = create();
    if (adapterHint != null) result.adapterHint = adapterHint;
    return result;
  }

  ChannelReceiveRequest._();

  factory ChannelReceiveRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ChannelReceiveRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ChannelReceiveRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'adapterHint')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelReceiveRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelReceiveRequest copyWith(
          void Function(ChannelReceiveRequest) updates) =>
      super.copyWith((message) => updates(message as ChannelReceiveRequest))
          as ChannelReceiveRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ChannelReceiveRequest create() => ChannelReceiveRequest._();
  @$core.override
  ChannelReceiveRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ChannelReceiveRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ChannelReceiveRequest>(create);
  static ChannelReceiveRequest? _defaultInstance;

  /// Free-form, untrusted, log-only label for operator debugging.
  @$pb.TagNumber(1)
  $core.String get adapterHint => $_getSZ(0);
  @$pb.TagNumber(1)
  set adapterHint($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasAdapterHint() => $_has(0);
  @$pb.TagNumber(1)
  void clearAdapterHint() => $_clearField(1);
}

class SubscribeRequest extends $pb.GeneratedMessage {
  factory SubscribeRequest() => create();

  SubscribeRequest._();

  factory SubscribeRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory SubscribeRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'SubscribeRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SubscribeRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SubscribeRequest copyWith(void Function(SubscribeRequest) updates) =>
      super.copyWith((message) => updates(message as SubscribeRequest))
          as SubscribeRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static SubscribeRequest create() => SubscribeRequest._();
  @$core.override
  SubscribeRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static SubscribeRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<SubscribeRequest>(create);
  static SubscribeRequest? _defaultInstance;
}

class RedeemEnrollmentRequest extends $pb.GeneratedMessage {
  factory RedeemEnrollmentRequest({
    $core.String? enrollmentCode,
    $core.List<$core.int>? publicKey,
  }) {
    final result = create();
    if (enrollmentCode != null) result.enrollmentCode = enrollmentCode;
    if (publicKey != null) result.publicKey = publicKey;
    return result;
  }

  RedeemEnrollmentRequest._();

  factory RedeemEnrollmentRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory RedeemEnrollmentRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'RedeemEnrollmentRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'enrollmentCode')
    ..a<$core.List<$core.int>>(
        2, _omitFieldNames ? '' : 'publicKey', $pb.PbFieldType.OY)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  RedeemEnrollmentRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  RedeemEnrollmentRequest copyWith(
          void Function(RedeemEnrollmentRequest) updates) =>
      super.copyWith((message) => updates(message as RedeemEnrollmentRequest))
          as RedeemEnrollmentRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static RedeemEnrollmentRequest create() => RedeemEnrollmentRequest._();
  @$core.override
  RedeemEnrollmentRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static RedeemEnrollmentRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<RedeemEnrollmentRequest>(create);
  static RedeemEnrollmentRequest? _defaultInstance;

  /// One-time enrollment code minted by the controller. Encoded as a
  /// signed JWT carrying {workspace, device_name (Client CR name),
  /// code_id, exp}. Single-use: redemption clears the code from the
  /// Client CR's status.
  @$pb.TagNumber(1)
  $core.String get enrollmentCode => $_getSZ(0);
  @$pb.TagNumber(1)
  set enrollmentCode($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasEnrollmentCode() => $_has(0);
  @$pb.TagNumber(1)
  void clearEnrollmentCode() => $_clearField(1);

  /// Client-generated P-256 ECDSA public key, SEC1 uncompressed bytes
  /// (or DER-encoded SubjectPublicKeyInfo — verifier accepts both).
  /// The controller persists this on the Client CR's status.publicKey;
  /// subsequent requests sign each call with the matching private key.
  @$pb.TagNumber(2)
  $core.List<$core.int> get publicKey => $_getN(1);
  @$pb.TagNumber(2)
  set publicKey($core.List<$core.int> value) => $_setBytes(1, value);
  @$pb.TagNumber(2)
  $core.bool hasPublicKey() => $_has(1);
  @$pb.TagNumber(2)
  void clearPublicKey() => $_clearField(2);
}

class RedeemEnrollmentResponse extends $pb.GeneratedMessage {
  factory RedeemEnrollmentResponse({
    $core.String? clientName,
    $fixnum.Int64? enrolledAt,
  }) {
    final result = create();
    if (clientName != null) result.clientName = clientName;
    if (enrolledAt != null) result.enrolledAt = enrolledAt;
    return result;
  }

  RedeemEnrollmentResponse._();

  factory RedeemEnrollmentResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory RedeemEnrollmentResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'RedeemEnrollmentResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'clientName')
    ..aInt64(2, _omitFieldNames ? '' : 'enrolledAt')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  RedeemEnrollmentResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  RedeemEnrollmentResponse copyWith(
          void Function(RedeemEnrollmentResponse) updates) =>
      super.copyWith((message) => updates(message as RedeemEnrollmentResponse))
          as RedeemEnrollmentResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static RedeemEnrollmentResponse create() => RedeemEnrollmentResponse._();
  @$core.override
  RedeemEnrollmentResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static RedeemEnrollmentResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<RedeemEnrollmentResponse>(create);
  static RedeemEnrollmentResponse? _defaultInstance;

  /// Client CR name the enrollment was applied to. Echoed back so the
  /// client can confirm + display its registered identity.
  @$pb.TagNumber(1)
  $core.String get clientName => $_getSZ(0);
  @$pb.TagNumber(1)
  set clientName($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasClientName() => $_has(0);
  @$pb.TagNumber(1)
  void clearClientName() => $_clearField(1);

  /// Unix-seconds timestamp when the public key was registered.
  @$pb.TagNumber(2)
  $fixnum.Int64 get enrolledAt => $_getI64(1);
  @$pb.TagNumber(2)
  set enrolledAt($fixnum.Int64 value) => $_setInt64(1, value);
  @$pb.TagNumber(2)
  $core.bool hasEnrolledAt() => $_has(1);
  @$pb.TagNumber(2)
  void clearEnrolledAt() => $_clearField(2);
}

class ServerRequest extends $pb.GeneratedMessage {
  factory ServerRequest({
    $core.String? requestId,
    $core.String? method,
    $core.String? paramsJson,
  }) {
    final result = create();
    if (requestId != null) result.requestId = requestId;
    if (method != null) result.method = method;
    if (paramsJson != null) result.paramsJson = paramsJson;
    return result;
  }

  ServerRequest._();

  factory ServerRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ServerRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ServerRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'requestId')
    ..aOS(2, _omitFieldNames ? '' : 'method')
    ..aOS(3, _omitFieldNames ? '' : 'paramsJson')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ServerRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ServerRequest copyWith(void Function(ServerRequest) updates) =>
      super.copyWith((message) => updates(message as ServerRequest))
          as ServerRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ServerRequest create() => ServerRequest._();
  @$core.override
  ServerRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ServerRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ServerRequest>(create);
  static ServerRequest? _defaultInstance;

  /// Empty string → notification (fire-and-forget). The client MUST NOT
  /// emit a ClientResponse. Non-empty → the client MUST eventually
  /// respond with a ClientResponse echoing this request_id.
  @$pb.TagNumber(1)
  $core.String get requestId => $_getSZ(0);
  @$pb.TagNumber(1)
  set requestId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasRequestId() => $_has(0);
  @$pb.TagNumber(1)
  void clearRequestId() => $_clearField(1);

  /// Tool name. Must be in the client's last-advertised supported_methods
  /// set or the controller refuses to dispatch.
  @$pb.TagNumber(2)
  $core.String get method => $_getSZ(1);
  @$pb.TagNumber(2)
  set method($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasMethod() => $_has(1);
  @$pb.TagNumber(2)
  void clearMethod() => $_clearField(2);

  /// Opaque JSON object — same convention as ToolCall.input_json.
  @$pb.TagNumber(3)
  $core.String get paramsJson => $_getSZ(2);
  @$pb.TagNumber(3)
  set paramsJson($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasParamsJson() => $_has(2);
  @$pb.TagNumber(3)
  void clearParamsJson() => $_clearField(3);
}

class ClientResponse extends $pb.GeneratedMessage {
  factory ClientResponse({
    $core.String? requestId,
    $core.String? resultJson,
    ClientResponseError? error,
  }) {
    final result = create();
    if (requestId != null) result.requestId = requestId;
    if (resultJson != null) result.resultJson = resultJson;
    if (error != null) result.error = error;
    return result;
  }

  ClientResponse._();

  factory ClientResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ClientResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ClientResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'requestId')
    ..aOS(2, _omitFieldNames ? '' : 'resultJson')
    ..aOM<ClientResponseError>(3, _omitFieldNames ? '' : 'error',
        subBuilder: ClientResponseError.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ClientResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ClientResponse copyWith(void Function(ClientResponse) updates) =>
      super.copyWith((message) => updates(message as ClientResponse))
          as ClientResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ClientResponse create() => ClientResponse._();
  @$core.override
  ClientResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ClientResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ClientResponse>(create);
  static ClientResponse? _defaultInstance;

  /// Echoes the originating ServerRequest.request_id.
  @$pb.TagNumber(1)
  $core.String get requestId => $_getSZ(0);
  @$pb.TagNumber(1)
  set requestId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasRequestId() => $_has(0);
  @$pb.TagNumber(1)
  void clearRequestId() => $_clearField(1);

  /// Exactly one of result_json / error MUST be set. Both-set or
  /// neither-set → InvalidArgument at the handler.
  @$pb.TagNumber(2)
  $core.String get resultJson => $_getSZ(1);
  @$pb.TagNumber(2)
  set resultJson($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasResultJson() => $_has(1);
  @$pb.TagNumber(2)
  void clearResultJson() => $_clearField(2);

  @$pb.TagNumber(3)
  ClientResponseError get error => $_getN(2);
  @$pb.TagNumber(3)
  set error(ClientResponseError value) => $_setField(3, value);
  @$pb.TagNumber(3)
  $core.bool hasError() => $_has(2);
  @$pb.TagNumber(3)
  void clearError() => $_clearField(3);
  @$pb.TagNumber(3)
  ClientResponseError ensureError() => $_ensure(2);
}

class ClientResponseError extends $pb.GeneratedMessage {
  factory ClientResponseError({
    $core.int? code,
    $core.String? message,
  }) {
    final result = create();
    if (code != null) result.code = code;
    if (message != null) result.message = message;
    return result;
  }

  ClientResponseError._();

  factory ClientResponseError.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ClientResponseError.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ClientResponseError',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aI(1, _omitFieldNames ? '' : 'code')
    ..aOS(2, _omitFieldNames ? '' : 'message')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ClientResponseError clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ClientResponseError copyWith(void Function(ClientResponseError) updates) =>
      super.copyWith((message) => updates(message as ClientResponseError))
          as ClientResponseError;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ClientResponseError create() => ClientResponseError._();
  @$core.override
  ClientResponseError createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ClientResponseError getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ClientResponseError>(create);
  static ClientResponseError? _defaultInstance;

  @$pb.TagNumber(1)
  $core.int get code => $_getIZ(0);
  @$pb.TagNumber(1)
  set code($core.int value) => $_setSignedInt32(0, value);
  @$pb.TagNumber(1)
  $core.bool hasCode() => $_has(0);
  @$pb.TagNumber(1)
  void clearCode() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get message => $_getSZ(1);
  @$pb.TagNumber(2)
  set message($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasMessage() => $_has(1);
  @$pb.TagNumber(2)
  void clearMessage() => $_clearField(2);
}

class SendServerNotificationRequest extends $pb.GeneratedMessage {
  factory SendServerNotificationRequest({
    $core.String? channelId,
    $core.String? method,
    $core.String? paramsJson,
  }) {
    final result = create();
    if (channelId != null) result.channelId = channelId;
    if (method != null) result.method = method;
    if (paramsJson != null) result.paramsJson = paramsJson;
    return result;
  }

  SendServerNotificationRequest._();

  factory SendServerNotificationRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory SendServerNotificationRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'SendServerNotificationRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'channelId')
    ..aOS(2, _omitFieldNames ? '' : 'method')
    ..aOS(3, _omitFieldNames ? '' : 'paramsJson')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SendServerNotificationRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SendServerNotificationRequest copyWith(
          void Function(SendServerNotificationRequest) updates) =>
      super.copyWith(
              (message) => updates(message as SendServerNotificationRequest))
          as SendServerNotificationRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static SendServerNotificationRequest create() =>
      SendServerNotificationRequest._();
  @$core.override
  SendServerNotificationRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static SendServerNotificationRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<SendServerNotificationRequest>(create);
  static SendServerNotificationRequest? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get channelId => $_getSZ(0);
  @$pb.TagNumber(1)
  set channelId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasChannelId() => $_has(0);
  @$pb.TagNumber(1)
  void clearChannelId() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get method => $_getSZ(1);
  @$pb.TagNumber(2)
  set method($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasMethod() => $_has(1);
  @$pb.TagNumber(2)
  void clearMethod() => $_clearField(2);

  @$pb.TagNumber(3)
  $core.String get paramsJson => $_getSZ(2);
  @$pb.TagNumber(3)
  set paramsJson($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasParamsJson() => $_has(2);
  @$pb.TagNumber(3)
  void clearParamsJson() => $_clearField(3);
}

class SendServerNotificationResponse extends $pb.GeneratedMessage {
  factory SendServerNotificationResponse({
    $core.bool? delivered,
  }) {
    final result = create();
    if (delivered != null) result.delivered = delivered;
    return result;
  }

  SendServerNotificationResponse._();

  factory SendServerNotificationResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory SendServerNotificationResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'SendServerNotificationResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOB(1, _omitFieldNames ? '' : 'delivered')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SendServerNotificationResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SendServerNotificationResponse copyWith(
          void Function(SendServerNotificationResponse) updates) =>
      super.copyWith(
              (message) => updates(message as SendServerNotificationResponse))
          as SendServerNotificationResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static SendServerNotificationResponse create() =>
      SendServerNotificationResponse._();
  @$core.override
  SendServerNotificationResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static SendServerNotificationResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<SendServerNotificationResponse>(create);
  static SendServerNotificationResponse? _defaultInstance;

  /// Best-effort: true if the frame was enqueued for delivery on the
  /// channel's mpsc. False on unknown channel or unsupported method (the
  /// client did not advertise the method in supported_methods).
  @$pb.TagNumber(1)
  $core.bool get delivered => $_getBF(0);
  @$pb.TagNumber(1)
  set delivered($core.bool value) => $_setBool(0, value);
  @$pb.TagNumber(1)
  $core.bool hasDelivered() => $_has(0);
  @$pb.TagNumber(1)
  void clearDelivered() => $_clearField(1);
}

class SendServerRequestAndAwaitRequest extends $pb.GeneratedMessage {
  factory SendServerRequestAndAwaitRequest({
    $core.String? channelId,
    $core.String? requestId,
    $core.String? method,
    $core.String? paramsJson,
    $core.int? timeoutSeconds,
  }) {
    final result = create();
    if (channelId != null) result.channelId = channelId;
    if (requestId != null) result.requestId = requestId;
    if (method != null) result.method = method;
    if (paramsJson != null) result.paramsJson = paramsJson;
    if (timeoutSeconds != null) result.timeoutSeconds = timeoutSeconds;
    return result;
  }

  SendServerRequestAndAwaitRequest._();

  factory SendServerRequestAndAwaitRequest.fromBuffer(
          $core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory SendServerRequestAndAwaitRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'SendServerRequestAndAwaitRequest',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'channelId')
    ..aOS(2, _omitFieldNames ? '' : 'requestId')
    ..aOS(3, _omitFieldNames ? '' : 'method')
    ..aOS(4, _omitFieldNames ? '' : 'paramsJson')
    ..aI(5, _omitFieldNames ? '' : 'timeoutSeconds',
        fieldType: $pb.PbFieldType.OU3)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SendServerRequestAndAwaitRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SendServerRequestAndAwaitRequest copyWith(
          void Function(SendServerRequestAndAwaitRequest) updates) =>
      super.copyWith(
              (message) => updates(message as SendServerRequestAndAwaitRequest))
          as SendServerRequestAndAwaitRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static SendServerRequestAndAwaitRequest create() =>
      SendServerRequestAndAwaitRequest._();
  @$core.override
  SendServerRequestAndAwaitRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static SendServerRequestAndAwaitRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<SendServerRequestAndAwaitRequest>(
          create);
  static SendServerRequestAndAwaitRequest? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get channelId => $_getSZ(0);
  @$pb.TagNumber(1)
  set channelId($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasChannelId() => $_has(0);
  @$pb.TagNumber(1)
  void clearChannelId() => $_clearField(1);

  /// Caller-supplied correlation id. Transponder uses the LLM's
  /// tool_call_id so the agent loop carries one identifier.
  @$pb.TagNumber(2)
  $core.String get requestId => $_getSZ(1);
  @$pb.TagNumber(2)
  set requestId($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasRequestId() => $_has(1);
  @$pb.TagNumber(2)
  void clearRequestId() => $_clearField(2);

  @$pb.TagNumber(3)
  $core.String get method => $_getSZ(2);
  @$pb.TagNumber(3)
  set method($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasMethod() => $_has(2);
  @$pb.TagNumber(3)
  void clearMethod() => $_clearField(3);

  @$pb.TagNumber(4)
  $core.String get paramsJson => $_getSZ(3);
  @$pb.TagNumber(4)
  set paramsJson($core.String value) => $_setString(3, value);
  @$pb.TagNumber(4)
  $core.bool hasParamsJson() => $_has(3);
  @$pb.TagNumber(4)
  void clearParamsJson() => $_clearField(4);

  /// Server-side wait cap in seconds. The controller clamps to a sane
  /// range. Zero falls back to a server default.
  @$pb.TagNumber(5)
  $core.int get timeoutSeconds => $_getIZ(4);
  @$pb.TagNumber(5)
  set timeoutSeconds($core.int value) => $_setUnsignedInt32(4, value);
  @$pb.TagNumber(5)
  $core.bool hasTimeoutSeconds() => $_has(4);
  @$pb.TagNumber(5)
  void clearTimeoutSeconds() => $_clearField(5);
}

class SendServerRequestAndAwaitResponse extends $pb.GeneratedMessage {
  factory SendServerRequestAndAwaitResponse({
    $core.String? resultJson,
    ClientResponseError? error,
    $core.bool? timedOut,
    $core.bool? unknownChannel,
    $core.bool? unsupportedMethod,
  }) {
    final result = create();
    if (resultJson != null) result.resultJson = resultJson;
    if (error != null) result.error = error;
    if (timedOut != null) result.timedOut = timedOut;
    if (unknownChannel != null) result.unknownChannel = unknownChannel;
    if (unsupportedMethod != null) result.unsupportedMethod = unsupportedMethod;
    return result;
  }

  SendServerRequestAndAwaitResponse._();

  factory SendServerRequestAndAwaitResponse.fromBuffer(
          $core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory SendServerRequestAndAwaitResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'SendServerRequestAndAwaitResponse',
      package:
          const $pb.PackageName(_omitMessageNames ? '' : 'sycophant.common.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'resultJson')
    ..aOM<ClientResponseError>(2, _omitFieldNames ? '' : 'error',
        subBuilder: ClientResponseError.create)
    ..aOB(3, _omitFieldNames ? '' : 'timedOut')
    ..aOB(4, _omitFieldNames ? '' : 'unknownChannel')
    ..aOB(5, _omitFieldNames ? '' : 'unsupportedMethod')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SendServerRequestAndAwaitResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  SendServerRequestAndAwaitResponse copyWith(
          void Function(SendServerRequestAndAwaitResponse) updates) =>
      super.copyWith((message) =>
              updates(message as SendServerRequestAndAwaitResponse))
          as SendServerRequestAndAwaitResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static SendServerRequestAndAwaitResponse create() =>
      SendServerRequestAndAwaitResponse._();
  @$core.override
  SendServerRequestAndAwaitResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static SendServerRequestAndAwaitResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<SendServerRequestAndAwaitResponse>(
          create);
  static SendServerRequestAndAwaitResponse? _defaultInstance;

  /// Exactly one of result_json / error / timed_out / unknown_channel /
  /// unsupported_method MUST be set on success-shaped outcomes.
  @$pb.TagNumber(1)
  $core.String get resultJson => $_getSZ(0);
  @$pb.TagNumber(1)
  set resultJson($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasResultJson() => $_has(0);
  @$pb.TagNumber(1)
  void clearResultJson() => $_clearField(1);

  @$pb.TagNumber(2)
  ClientResponseError get error => $_getN(1);
  @$pb.TagNumber(2)
  set error(ClientResponseError value) => $_setField(2, value);
  @$pb.TagNumber(2)
  $core.bool hasError() => $_has(1);
  @$pb.TagNumber(2)
  void clearError() => $_clearField(2);
  @$pb.TagNumber(2)
  ClientResponseError ensureError() => $_ensure(1);

  @$pb.TagNumber(3)
  $core.bool get timedOut => $_getBF(2);
  @$pb.TagNumber(3)
  set timedOut($core.bool value) => $_setBool(2, value);
  @$pb.TagNumber(3)
  $core.bool hasTimedOut() => $_has(2);
  @$pb.TagNumber(3)
  void clearTimedOut() => $_clearField(3);

  @$pb.TagNumber(4)
  $core.bool get unknownChannel => $_getBF(3);
  @$pb.TagNumber(4)
  set unknownChannel($core.bool value) => $_setBool(3, value);
  @$pb.TagNumber(4)
  $core.bool hasUnknownChannel() => $_has(3);
  @$pb.TagNumber(4)
  void clearUnknownChannel() => $_clearField(4);

  @$pb.TagNumber(5)
  $core.bool get unsupportedMethod => $_getBF(4);
  @$pb.TagNumber(5)
  set unsupportedMethod($core.bool value) => $_setBool(4, value);
  @$pb.TagNumber(5)
  $core.bool hasUnsupportedMethod() => $_has(4);
  @$pb.TagNumber(5)
  void clearUnsupportedMethod() => $_clearField(5);
}

const $core.bool _omitFieldNames =
    $core.bool.fromEnvironment('protobuf.omit_field_names');
const $core.bool _omitMessageNames =
    $core.bool.fromEnvironment('protobuf.omit_message_names');
