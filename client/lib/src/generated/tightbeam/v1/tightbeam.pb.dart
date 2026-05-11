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

import 'package:fixnum/fixnum.dart' as $fixnum;
import 'package:protobuf/protobuf.dart' as $pb;

import 'tightbeam.pbenum.dart';

export 'package:protobuf/protobuf.dart' show GeneratedMessageGenericExtensions;

export 'tightbeam.pbenum.dart';

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
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
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
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
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
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
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
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
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
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
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
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
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
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
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

class GetTurnRequest extends $pb.GeneratedMessage {
  factory GetTurnRequest({
    $core.String? modelName,
  }) {
    final result = create();
    if (modelName != null) result.modelName = modelName;
    return result;
  }

  GetTurnRequest._();

  factory GetTurnRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory GetTurnRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'GetTurnRequest',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'modelName')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  GetTurnRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  GetTurnRequest copyWith(void Function(GetTurnRequest) updates) =>
      super.copyWith((message) => updates(message as GetTurnRequest))
          as GetTurnRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static GetTurnRequest create() => GetTurnRequest._();
  @$core.override
  GetTurnRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static GetTurnRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<GetTurnRequest>(create);
  static GetTurnRequest? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get modelName => $_getSZ(0);
  @$pb.TagNumber(1)
  set modelName($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasModelName() => $_has(0);
  @$pb.TagNumber(1)
  void clearModelName() => $_clearField(1);
}

class TurnAssignment extends $pb.GeneratedMessage {
  factory TurnAssignment({
    $core.String? system,
    $core.Iterable<ToolDefinition>? tools,
    $core.Iterable<Message>? messages,
    $core.String? paramsJson,
  }) {
    final result = create();
    if (system != null) result.system = system;
    if (tools != null) result.tools.addAll(tools);
    if (messages != null) result.messages.addAll(messages);
    if (paramsJson != null) result.paramsJson = paramsJson;
    return result;
  }

  TurnAssignment._();

  factory TurnAssignment.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory TurnAssignment.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'TurnAssignment',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'system')
    ..pPM<ToolDefinition>(2, _omitFieldNames ? '' : 'tools',
        subBuilder: ToolDefinition.create)
    ..pPM<Message>(3, _omitFieldNames ? '' : 'messages',
        subBuilder: Message.create)
    ..aOS(4, _omitFieldNames ? '' : 'paramsJson')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnAssignment clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnAssignment copyWith(void Function(TurnAssignment) updates) =>
      super.copyWith((message) => updates(message as TurnAssignment))
          as TurnAssignment;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static TurnAssignment create() => TurnAssignment._();
  @$core.override
  TurnAssignment createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static TurnAssignment getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<TurnAssignment>(create);
  static TurnAssignment? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get system => $_getSZ(0);
  @$pb.TagNumber(1)
  set system($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasSystem() => $_has(0);
  @$pb.TagNumber(1)
  void clearSystem() => $_clearField(1);

  @$pb.TagNumber(2)
  $pb.PbList<ToolDefinition> get tools => $_getList(1);

  @$pb.TagNumber(3)
  $pb.PbList<Message> get messages => $_getList(2);

  /// RFC 7396-merged params blob (TightbeamModel.params with frontmatter
  /// override applied). Opaque to sycophant; merged into provider request body.
  @$pb.TagNumber(4)
  $core.String get paramsJson => $_getSZ(3);
  @$pb.TagNumber(4)
  set paramsJson($core.String value) => $_setString(3, value);
  @$pb.TagNumber(4)
  $core.bool hasParamsJson() => $_has(3);
  @$pb.TagNumber(4)
  void clearParamsJson() => $_clearField(4);
}

enum TurnResultChunk_Chunk {
  contentDelta,
  toolUseStart,
  toolUseInput,
  complete,
  error,
  warning,
  notSet
}

class TurnResultChunk extends $pb.GeneratedMessage {
  factory TurnResultChunk({
    ContentDelta? contentDelta,
    ToolUseStart? toolUseStart,
    ToolUseInput? toolUseInput,
    TurnComplete? complete,
    TurnError? error,
    TurnWarning? warning,
  }) {
    final result = create();
    if (contentDelta != null) result.contentDelta = contentDelta;
    if (toolUseStart != null) result.toolUseStart = toolUseStart;
    if (toolUseInput != null) result.toolUseInput = toolUseInput;
    if (complete != null) result.complete = complete;
    if (error != null) result.error = error;
    if (warning != null) result.warning = warning;
    return result;
  }

  TurnResultChunk._();

  factory TurnResultChunk.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory TurnResultChunk.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static const $core.Map<$core.int, TurnResultChunk_Chunk>
      _TurnResultChunk_ChunkByTag = {
    1: TurnResultChunk_Chunk.contentDelta,
    2: TurnResultChunk_Chunk.toolUseStart,
    3: TurnResultChunk_Chunk.toolUseInput,
    4: TurnResultChunk_Chunk.complete,
    5: TurnResultChunk_Chunk.error,
    6: TurnResultChunk_Chunk.warning,
    0: TurnResultChunk_Chunk.notSet
  };
  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'TurnResultChunk',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..oo(0, [1, 2, 3, 4, 5, 6])
    ..aOM<ContentDelta>(1, _omitFieldNames ? '' : 'contentDelta',
        subBuilder: ContentDelta.create)
    ..aOM<ToolUseStart>(2, _omitFieldNames ? '' : 'toolUseStart',
        subBuilder: ToolUseStart.create)
    ..aOM<ToolUseInput>(3, _omitFieldNames ? '' : 'toolUseInput',
        subBuilder: ToolUseInput.create)
    ..aOM<TurnComplete>(4, _omitFieldNames ? '' : 'complete',
        subBuilder: TurnComplete.create)
    ..aOM<TurnError>(5, _omitFieldNames ? '' : 'error',
        subBuilder: TurnError.create)
    ..aOM<TurnWarning>(6, _omitFieldNames ? '' : 'warning',
        subBuilder: TurnWarning.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnResultChunk clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnResultChunk copyWith(void Function(TurnResultChunk) updates) =>
      super.copyWith((message) => updates(message as TurnResultChunk))
          as TurnResultChunk;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static TurnResultChunk create() => TurnResultChunk._();
  @$core.override
  TurnResultChunk createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static TurnResultChunk getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<TurnResultChunk>(create);
  static TurnResultChunk? _defaultInstance;

  @$pb.TagNumber(1)
  @$pb.TagNumber(2)
  @$pb.TagNumber(3)
  @$pb.TagNumber(4)
  @$pb.TagNumber(5)
  @$pb.TagNumber(6)
  TurnResultChunk_Chunk whichChunk() =>
      _TurnResultChunk_ChunkByTag[$_whichOneof(0)]!;
  @$pb.TagNumber(1)
  @$pb.TagNumber(2)
  @$pb.TagNumber(3)
  @$pb.TagNumber(4)
  @$pb.TagNumber(5)
  @$pb.TagNumber(6)
  void clearChunk() => $_clearField($_whichOneof(0));

  @$pb.TagNumber(1)
  ContentDelta get contentDelta => $_getN(0);
  @$pb.TagNumber(1)
  set contentDelta(ContentDelta value) => $_setField(1, value);
  @$pb.TagNumber(1)
  $core.bool hasContentDelta() => $_has(0);
  @$pb.TagNumber(1)
  void clearContentDelta() => $_clearField(1);
  @$pb.TagNumber(1)
  ContentDelta ensureContentDelta() => $_ensure(0);

  @$pb.TagNumber(2)
  ToolUseStart get toolUseStart => $_getN(1);
  @$pb.TagNumber(2)
  set toolUseStart(ToolUseStart value) => $_setField(2, value);
  @$pb.TagNumber(2)
  $core.bool hasToolUseStart() => $_has(1);
  @$pb.TagNumber(2)
  void clearToolUseStart() => $_clearField(2);
  @$pb.TagNumber(2)
  ToolUseStart ensureToolUseStart() => $_ensure(1);

  @$pb.TagNumber(3)
  ToolUseInput get toolUseInput => $_getN(2);
  @$pb.TagNumber(3)
  set toolUseInput(ToolUseInput value) => $_setField(3, value);
  @$pb.TagNumber(3)
  $core.bool hasToolUseInput() => $_has(2);
  @$pb.TagNumber(3)
  void clearToolUseInput() => $_clearField(3);
  @$pb.TagNumber(3)
  ToolUseInput ensureToolUseInput() => $_ensure(2);

  @$pb.TagNumber(4)
  TurnComplete get complete => $_getN(3);
  @$pb.TagNumber(4)
  set complete(TurnComplete value) => $_setField(4, value);
  @$pb.TagNumber(4)
  $core.bool hasComplete() => $_has(3);
  @$pb.TagNumber(4)
  void clearComplete() => $_clearField(4);
  @$pb.TagNumber(4)
  TurnComplete ensureComplete() => $_ensure(3);

  @$pb.TagNumber(5)
  TurnError get error => $_getN(4);
  @$pb.TagNumber(5)
  set error(TurnError value) => $_setField(5, value);
  @$pb.TagNumber(5)
  $core.bool hasError() => $_has(4);
  @$pb.TagNumber(5)
  void clearError() => $_clearField(5);
  @$pb.TagNumber(5)
  TurnError ensureError() => $_ensure(4);

  @$pb.TagNumber(6)
  TurnWarning get warning => $_getN(5);
  @$pb.TagNumber(6)
  set warning(TurnWarning value) => $_setField(6, value);
  @$pb.TagNumber(6)
  $core.bool hasWarning() => $_has(5);
  @$pb.TagNumber(6)
  void clearWarning() => $_clearField(6);
  @$pb.TagNumber(6)
  TurnWarning ensureWarning() => $_ensure(5);
}

class ContentDelta extends $pb.GeneratedMessage {
  factory ContentDelta({
    $core.String? text,
  }) {
    final result = create();
    if (text != null) result.text = text;
    return result;
  }

  ContentDelta._();

  factory ContentDelta.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ContentDelta.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ContentDelta',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'text')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ContentDelta clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ContentDelta copyWith(void Function(ContentDelta) updates) =>
      super.copyWith((message) => updates(message as ContentDelta))
          as ContentDelta;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ContentDelta create() => ContentDelta._();
  @$core.override
  ContentDelta createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ContentDelta getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ContentDelta>(create);
  static ContentDelta? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get text => $_getSZ(0);
  @$pb.TagNumber(1)
  set text($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasText() => $_has(0);
  @$pb.TagNumber(1)
  void clearText() => $_clearField(1);
}

class ToolUseStart extends $pb.GeneratedMessage {
  factory ToolUseStart({
    $core.String? id,
    $core.String? name,
  }) {
    final result = create();
    if (id != null) result.id = id;
    if (name != null) result.name = name;
    return result;
  }

  ToolUseStart._();

  factory ToolUseStart.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ToolUseStart.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ToolUseStart',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'id')
    ..aOS(2, _omitFieldNames ? '' : 'name')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolUseStart clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolUseStart copyWith(void Function(ToolUseStart) updates) =>
      super.copyWith((message) => updates(message as ToolUseStart))
          as ToolUseStart;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ToolUseStart create() => ToolUseStart._();
  @$core.override
  ToolUseStart createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ToolUseStart getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ToolUseStart>(create);
  static ToolUseStart? _defaultInstance;

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
}

class ToolUseInput extends $pb.GeneratedMessage {
  factory ToolUseInput({
    $core.String? partialJson,
  }) {
    final result = create();
    if (partialJson != null) result.partialJson = partialJson;
    return result;
  }

  ToolUseInput._();

  factory ToolUseInput.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ToolUseInput.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ToolUseInput',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'partialJson')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolUseInput clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ToolUseInput copyWith(void Function(ToolUseInput) updates) =>
      super.copyWith((message) => updates(message as ToolUseInput))
          as ToolUseInput;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ToolUseInput create() => ToolUseInput._();
  @$core.override
  ToolUseInput createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ToolUseInput getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ToolUseInput>(create);
  static ToolUseInput? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get partialJson => $_getSZ(0);
  @$pb.TagNumber(1)
  set partialJson($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasPartialJson() => $_has(0);
  @$pb.TagNumber(1)
  void clearPartialJson() => $_clearField(1);
}

class TurnComplete extends $pb.GeneratedMessage {
  factory TurnComplete({
    StopReason? stopReason,
    $core.Iterable<ContentBlock>? content,
    $core.Iterable<ToolCall>? toolCalls,
  }) {
    final result = create();
    if (stopReason != null) result.stopReason = stopReason;
    if (content != null) result.content.addAll(content);
    if (toolCalls != null) result.toolCalls.addAll(toolCalls);
    return result;
  }

  TurnComplete._();

  factory TurnComplete.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory TurnComplete.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'TurnComplete',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aE<StopReason>(1, _omitFieldNames ? '' : 'stopReason',
        enumValues: StopReason.values)
    ..pPM<ContentBlock>(2, _omitFieldNames ? '' : 'content',
        subBuilder: ContentBlock.create)
    ..pPM<ToolCall>(3, _omitFieldNames ? '' : 'toolCalls',
        subBuilder: ToolCall.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnComplete clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnComplete copyWith(void Function(TurnComplete) updates) =>
      super.copyWith((message) => updates(message as TurnComplete))
          as TurnComplete;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static TurnComplete create() => TurnComplete._();
  @$core.override
  TurnComplete createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static TurnComplete getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<TurnComplete>(create);
  static TurnComplete? _defaultInstance;

  @$pb.TagNumber(1)
  StopReason get stopReason => $_getN(0);
  @$pb.TagNumber(1)
  set stopReason(StopReason value) => $_setField(1, value);
  @$pb.TagNumber(1)
  $core.bool hasStopReason() => $_has(0);
  @$pb.TagNumber(1)
  void clearStopReason() => $_clearField(1);

  @$pb.TagNumber(2)
  $pb.PbList<ContentBlock> get content => $_getList(1);

  @$pb.TagNumber(3)
  $pb.PbList<ToolCall> get toolCalls => $_getList(2);
}

class TurnError extends $pb.GeneratedMessage {
  factory TurnError({
    $core.int? code,
    $core.String? message,
  }) {
    final result = create();
    if (code != null) result.code = code;
    if (message != null) result.message = message;
    return result;
  }

  TurnError._();

  factory TurnError.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory TurnError.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'TurnError',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aI(1, _omitFieldNames ? '' : 'code')
    ..aOS(2, _omitFieldNames ? '' : 'message')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnError clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnError copyWith(void Function(TurnError) updates) =>
      super.copyWith((message) => updates(message as TurnError)) as TurnError;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static TurnError create() => TurnError._();
  @$core.override
  TurnError createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static TurnError getDefault() =>
      _defaultInstance ??= $pb.GeneratedMessage.$_defaultFor<TurnError>(create);
  static TurnError? _defaultInstance;

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

/// Reports a principal-supplied params field that was overwritten by sycophant
/// because it is operator-bound (the managed-fields list per format).
class TurnWarning extends $pb.GeneratedMessage {
  factory TurnWarning({
    $core.String? field_1,
    $core.String? reason,
  }) {
    final result = create();
    if (field_1 != null) result.field_1 = field_1;
    if (reason != null) result.reason = reason;
    return result;
  }

  TurnWarning._();

  factory TurnWarning.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory TurnWarning.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'TurnWarning',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'field')
    ..aOS(2, _omitFieldNames ? '' : 'reason')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnWarning clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnWarning copyWith(void Function(TurnWarning) updates) =>
      super.copyWith((message) => updates(message as TurnWarning))
          as TurnWarning;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static TurnWarning create() => TurnWarning._();
  @$core.override
  TurnWarning createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static TurnWarning getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<TurnWarning>(create);
  static TurnWarning? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get field_1 => $_getSZ(0);
  @$pb.TagNumber(1)
  set field_1($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasField_1() => $_has(0);
  @$pb.TagNumber(1)
  void clearField_1() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get reason => $_getSZ(1);
  @$pb.TagNumber(2)
  set reason($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasReason() => $_has(1);
  @$pb.TagNumber(2)
  void clearReason() => $_clearField(2);
}

class TurnAck extends $pb.GeneratedMessage {
  factory TurnAck() => create();

  TurnAck._();

  factory TurnAck.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory TurnAck.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'TurnAck',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnAck clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnAck copyWith(void Function(TurnAck) updates) =>
      super.copyWith((message) => updates(message as TurnAck)) as TurnAck;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static TurnAck create() => TurnAck._();
  @$core.override
  TurnAck createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static TurnAck getDefault() =>
      _defaultInstance ??= $pb.GeneratedMessage.$_defaultFor<TurnAck>(create);
  static TurnAck? _defaultInstance;
}

class TurnRequest extends $pb.GeneratedMessage {
  factory TurnRequest({
    $core.String? system,
    $core.Iterable<ToolDefinition>? tools,
    $core.Iterable<Message>? messages,
    $core.String? model,
    $core.String? replyChannel,
    TurnRole? role,
    $core.String? correlationId,
  }) {
    final result = create();
    if (system != null) result.system = system;
    if (tools != null) result.tools.addAll(tools);
    if (messages != null) result.messages.addAll(messages);
    if (model != null) result.model = model;
    if (replyChannel != null) result.replyChannel = replyChannel;
    if (role != null) result.role = role;
    if (correlationId != null) result.correlationId = correlationId;
    return result;
  }

  TurnRequest._();

  factory TurnRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory TurnRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'TurnRequest',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'system')
    ..pPM<ToolDefinition>(2, _omitFieldNames ? '' : 'tools',
        subBuilder: ToolDefinition.create)
    ..pPM<Message>(3, _omitFieldNames ? '' : 'messages',
        subBuilder: Message.create)
    ..aOS(5, _omitFieldNames ? '' : 'model')
    ..aOS(6, _omitFieldNames ? '' : 'replyChannel')
    ..aE<TurnRole>(7, _omitFieldNames ? '' : 'role',
        enumValues: TurnRole.values)
    ..aOS(9, _omitFieldNames ? '' : 'correlationId')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnRequest copyWith(void Function(TurnRequest) updates) =>
      super.copyWith((message) => updates(message as TurnRequest))
          as TurnRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static TurnRequest create() => TurnRequest._();
  @$core.override
  TurnRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static TurnRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<TurnRequest>(create);
  static TurnRequest? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get system => $_getSZ(0);
  @$pb.TagNumber(1)
  set system($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasSystem() => $_has(0);
  @$pb.TagNumber(1)
  void clearSystem() => $_clearField(1);

  @$pb.TagNumber(2)
  $pb.PbList<ToolDefinition> get tools => $_getList(1);

  @$pb.TagNumber(3)
  $pb.PbList<Message> get messages => $_getList(2);

  @$pb.TagNumber(5)
  $core.String get model => $_getSZ(3);
  @$pb.TagNumber(5)
  set model($core.String value) => $_setString(3, value);
  @$pb.TagNumber(5)
  $core.bool hasModel() => $_has(3);
  @$pb.TagNumber(5)
  void clearModel() => $_clearField(5);

  @$pb.TagNumber(6)
  $core.String get replyChannel => $_getSZ(4);
  @$pb.TagNumber(6)
  set replyChannel($core.String value) => $_setString(4, value);
  @$pb.TagNumber(6)
  $core.bool hasReplyChannel() => $_has(4);
  @$pb.TagNumber(6)
  void clearReplyChannel() => $_clearField(6);

  /// role tags conversation log entries. DELEGATE entries are scoped per
  /// delegate call via correlation_id; orchestrator turns leave role unset.
  @$pb.TagNumber(7)
  TurnRole get role => $_getN(5);
  @$pb.TagNumber(7)
  set role(TurnRole value) => $_setField(7, value);
  @$pb.TagNumber(7)
  $core.bool hasRole() => $_has(5);
  @$pb.TagNumber(7)
  void clearRole() => $_clearField(7);

  /// Per-call correlation id for delegate turns. Set to the orchestrator's
  /// tool_use_id so each delegate's entries can be tagged "delegate:<id>"
  /// and scoped independently in history_for_provider. None for orchestrator
  /// turns.
  @$pb.TagNumber(9)
  $core.String get correlationId => $_getSZ(6);
  @$pb.TagNumber(9)
  set correlationId($core.String value) => $_setString(6, value);
  @$pb.TagNumber(9)
  $core.bool hasCorrelationId() => $_has(6);
  @$pb.TagNumber(9)
  void clearCorrelationId() => $_clearField(9);
}

enum TurnEvent_Event {
  contentDelta,
  toolUseStart,
  toolUseInput,
  complete,
  error,
  warning,
  notSet
}

class TurnEvent extends $pb.GeneratedMessage {
  factory TurnEvent({
    ContentDelta? contentDelta,
    ToolUseStart? toolUseStart,
    ToolUseInput? toolUseInput,
    TurnComplete? complete,
    TurnError? error,
    TurnWarning? warning,
  }) {
    final result = create();
    if (contentDelta != null) result.contentDelta = contentDelta;
    if (toolUseStart != null) result.toolUseStart = toolUseStart;
    if (toolUseInput != null) result.toolUseInput = toolUseInput;
    if (complete != null) result.complete = complete;
    if (error != null) result.error = error;
    if (warning != null) result.warning = warning;
    return result;
  }

  TurnEvent._();

  factory TurnEvent.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory TurnEvent.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static const $core.Map<$core.int, TurnEvent_Event> _TurnEvent_EventByTag = {
    1: TurnEvent_Event.contentDelta,
    2: TurnEvent_Event.toolUseStart,
    3: TurnEvent_Event.toolUseInput,
    4: TurnEvent_Event.complete,
    5: TurnEvent_Event.error,
    6: TurnEvent_Event.warning,
    0: TurnEvent_Event.notSet
  };
  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'TurnEvent',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..oo(0, [1, 2, 3, 4, 5, 6])
    ..aOM<ContentDelta>(1, _omitFieldNames ? '' : 'contentDelta',
        subBuilder: ContentDelta.create)
    ..aOM<ToolUseStart>(2, _omitFieldNames ? '' : 'toolUseStart',
        subBuilder: ToolUseStart.create)
    ..aOM<ToolUseInput>(3, _omitFieldNames ? '' : 'toolUseInput',
        subBuilder: ToolUseInput.create)
    ..aOM<TurnComplete>(4, _omitFieldNames ? '' : 'complete',
        subBuilder: TurnComplete.create)
    ..aOM<TurnError>(5, _omitFieldNames ? '' : 'error',
        subBuilder: TurnError.create)
    ..aOM<TurnWarning>(6, _omitFieldNames ? '' : 'warning',
        subBuilder: TurnWarning.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnEvent clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  TurnEvent copyWith(void Function(TurnEvent) updates) =>
      super.copyWith((message) => updates(message as TurnEvent)) as TurnEvent;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static TurnEvent create() => TurnEvent._();
  @$core.override
  TurnEvent createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static TurnEvent getDefault() =>
      _defaultInstance ??= $pb.GeneratedMessage.$_defaultFor<TurnEvent>(create);
  static TurnEvent? _defaultInstance;

  @$pb.TagNumber(1)
  @$pb.TagNumber(2)
  @$pb.TagNumber(3)
  @$pb.TagNumber(4)
  @$pb.TagNumber(5)
  @$pb.TagNumber(6)
  TurnEvent_Event whichEvent() => _TurnEvent_EventByTag[$_whichOneof(0)]!;
  @$pb.TagNumber(1)
  @$pb.TagNumber(2)
  @$pb.TagNumber(3)
  @$pb.TagNumber(4)
  @$pb.TagNumber(5)
  @$pb.TagNumber(6)
  void clearEvent() => $_clearField($_whichOneof(0));

  @$pb.TagNumber(1)
  ContentDelta get contentDelta => $_getN(0);
  @$pb.TagNumber(1)
  set contentDelta(ContentDelta value) => $_setField(1, value);
  @$pb.TagNumber(1)
  $core.bool hasContentDelta() => $_has(0);
  @$pb.TagNumber(1)
  void clearContentDelta() => $_clearField(1);
  @$pb.TagNumber(1)
  ContentDelta ensureContentDelta() => $_ensure(0);

  @$pb.TagNumber(2)
  ToolUseStart get toolUseStart => $_getN(1);
  @$pb.TagNumber(2)
  set toolUseStart(ToolUseStart value) => $_setField(2, value);
  @$pb.TagNumber(2)
  $core.bool hasToolUseStart() => $_has(1);
  @$pb.TagNumber(2)
  void clearToolUseStart() => $_clearField(2);
  @$pb.TagNumber(2)
  ToolUseStart ensureToolUseStart() => $_ensure(1);

  @$pb.TagNumber(3)
  ToolUseInput get toolUseInput => $_getN(2);
  @$pb.TagNumber(3)
  set toolUseInput(ToolUseInput value) => $_setField(3, value);
  @$pb.TagNumber(3)
  $core.bool hasToolUseInput() => $_has(2);
  @$pb.TagNumber(3)
  void clearToolUseInput() => $_clearField(3);
  @$pb.TagNumber(3)
  ToolUseInput ensureToolUseInput() => $_ensure(2);

  @$pb.TagNumber(4)
  TurnComplete get complete => $_getN(3);
  @$pb.TagNumber(4)
  set complete(TurnComplete value) => $_setField(4, value);
  @$pb.TagNumber(4)
  $core.bool hasComplete() => $_has(3);
  @$pb.TagNumber(4)
  void clearComplete() => $_clearField(4);
  @$pb.TagNumber(4)
  TurnComplete ensureComplete() => $_ensure(3);

  @$pb.TagNumber(5)
  TurnError get error => $_getN(4);
  @$pb.TagNumber(5)
  set error(TurnError value) => $_setField(5, value);
  @$pb.TagNumber(5)
  $core.bool hasError() => $_has(4);
  @$pb.TagNumber(5)
  void clearError() => $_clearField(5);
  @$pb.TagNumber(5)
  TurnError ensureError() => $_ensure(4);

  @$pb.TagNumber(6)
  TurnWarning get warning => $_getN(5);
  @$pb.TagNumber(6)
  set warning(TurnWarning value) => $_setField(6, value);
  @$pb.TagNumber(6)
  $core.bool hasWarning() => $_has(5);
  @$pb.TagNumber(6)
  void clearWarning() => $_clearField(6);
  @$pb.TagNumber(6)
  TurnWarning ensureWarning() => $_ensure(5);
}

class ListModelsRequest extends $pb.GeneratedMessage {
  factory ListModelsRequest() => create();

  ListModelsRequest._();

  factory ListModelsRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ListModelsRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ListModelsRequest',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListModelsRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListModelsRequest copyWith(void Function(ListModelsRequest) updates) =>
      super.copyWith((message) => updates(message as ListModelsRequest))
          as ListModelsRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ListModelsRequest create() => ListModelsRequest._();
  @$core.override
  ListModelsRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ListModelsRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ListModelsRequest>(create);
  static ListModelsRequest? _defaultInstance;
}

class ListModelsResponse extends $pb.GeneratedMessage {
  factory ListModelsResponse({
    $core.Iterable<ModelInfo>? models,
  }) {
    final result = create();
    if (models != null) result.models.addAll(models);
    return result;
  }

  ListModelsResponse._();

  factory ListModelsResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ListModelsResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ListModelsResponse',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..pPM<ModelInfo>(1, _omitFieldNames ? '' : 'models',
        subBuilder: ModelInfo.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListModelsResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ListModelsResponse copyWith(void Function(ListModelsResponse) updates) =>
      super.copyWith((message) => updates(message as ListModelsResponse))
          as ListModelsResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ListModelsResponse create() => ListModelsResponse._();
  @$core.override
  ListModelsResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ListModelsResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ListModelsResponse>(create);
  static ListModelsResponse? _defaultInstance;

  @$pb.TagNumber(1)
  $pb.PbList<ModelInfo> get models => $_getList(0);
}

class ModelInfo extends $pb.GeneratedMessage {
  factory ModelInfo({
    $core.String? name,
    $core.String? provider,
    $core.String? model,
    $core.String? description,
  }) {
    final result = create();
    if (name != null) result.name = name;
    if (provider != null) result.provider = provider;
    if (model != null) result.model = model;
    if (description != null) result.description = description;
    return result;
  }

  ModelInfo._();

  factory ModelInfo.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ModelInfo.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ModelInfo',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'name')
    ..aOS(2, _omitFieldNames ? '' : 'provider')
    ..aOS(3, _omitFieldNames ? '' : 'model')
    ..aOS(4, _omitFieldNames ? '' : 'description')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ModelInfo clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ModelInfo copyWith(void Function(ModelInfo) updates) =>
      super.copyWith((message) => updates(message as ModelInfo)) as ModelInfo;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ModelInfo create() => ModelInfo._();
  @$core.override
  ModelInfo createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ModelInfo getDefault() =>
      _defaultInstance ??= $pb.GeneratedMessage.$_defaultFor<ModelInfo>(create);
  static ModelInfo? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get name => $_getSZ(0);
  @$pb.TagNumber(1)
  set name($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasName() => $_has(0);
  @$pb.TagNumber(1)
  void clearName() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get provider => $_getSZ(1);
  @$pb.TagNumber(2)
  set provider($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasProvider() => $_has(1);
  @$pb.TagNumber(2)
  void clearProvider() => $_clearField(2);

  @$pb.TagNumber(3)
  $core.String get model => $_getSZ(2);
  @$pb.TagNumber(3)
  set model($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasModel() => $_has(2);
  @$pb.TagNumber(3)
  void clearModel() => $_clearField(3);

  @$pb.TagNumber(4)
  $core.String get description => $_getSZ(3);
  @$pb.TagNumber(4)
  set description($core.String value) => $_setString(3, value);
  @$pb.TagNumber(4)
  $core.bool hasDescription() => $_has(3);
  @$pb.TagNumber(4)
  void clearDescription() => $_clearField(4);
}

enum ChannelInbound_Event { register, userMessage, notSet }

class ChannelInbound extends $pb.GeneratedMessage {
  factory ChannelInbound({
    ChannelRegister? register,
    UserMessage? userMessage,
  }) {
    final result = create();
    if (register != null) result.register = register;
    if (userMessage != null) result.userMessage = userMessage;
    return result;
  }

  ChannelInbound._();

  factory ChannelInbound.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ChannelInbound.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static const $core.Map<$core.int, ChannelInbound_Event>
      _ChannelInbound_EventByTag = {
    1: ChannelInbound_Event.register,
    2: ChannelInbound_Event.userMessage,
    0: ChannelInbound_Event.notSet
  };
  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ChannelInbound',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..oo(0, [1, 2])
    ..aOM<ChannelRegister>(1, _omitFieldNames ? '' : 'register',
        subBuilder: ChannelRegister.create)
    ..aOM<UserMessage>(2, _omitFieldNames ? '' : 'userMessage',
        subBuilder: UserMessage.create)
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelInbound clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelInbound copyWith(void Function(ChannelInbound) updates) =>
      super.copyWith((message) => updates(message as ChannelInbound))
          as ChannelInbound;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ChannelInbound create() => ChannelInbound._();
  @$core.override
  ChannelInbound createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ChannelInbound getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ChannelInbound>(create);
  static ChannelInbound? _defaultInstance;

  @$pb.TagNumber(1)
  @$pb.TagNumber(2)
  ChannelInbound_Event whichEvent() =>
      _ChannelInbound_EventByTag[$_whichOneof(0)]!;
  @$pb.TagNumber(1)
  @$pb.TagNumber(2)
  void clearEvent() => $_clearField($_whichOneof(0));

  @$pb.TagNumber(1)
  ChannelRegister get register => $_getN(0);
  @$pb.TagNumber(1)
  set register(ChannelRegister value) => $_setField(1, value);
  @$pb.TagNumber(1)
  $core.bool hasRegister() => $_has(0);
  @$pb.TagNumber(1)
  void clearRegister() => $_clearField(1);
  @$pb.TagNumber(1)
  ChannelRegister ensureRegister() => $_ensure(0);

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
}

class ChannelRegister extends $pb.GeneratedMessage {
  factory ChannelRegister({
    $core.String? channelType,
    $core.String? channelName,
    $core.String? workspace,
  }) {
    final result = create();
    if (channelType != null) result.channelType = channelType;
    if (channelName != null) result.channelName = channelName;
    if (workspace != null) result.workspace = workspace;
    return result;
  }

  ChannelRegister._();

  factory ChannelRegister.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory ChannelRegister.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ChannelRegister',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'channelType')
    ..aOS(2, _omitFieldNames ? '' : 'channelName')
    ..aOS(3, _omitFieldNames ? '' : 'workspace')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelRegister clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  ChannelRegister copyWith(void Function(ChannelRegister) updates) =>
      super.copyWith((message) => updates(message as ChannelRegister))
          as ChannelRegister;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static ChannelRegister create() => ChannelRegister._();
  @$core.override
  ChannelRegister createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static ChannelRegister getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<ChannelRegister>(create);
  static ChannelRegister? _defaultInstance;

  @$pb.TagNumber(1)
  $core.String get channelType => $_getSZ(0);
  @$pb.TagNumber(1)
  set channelType($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasChannelType() => $_has(0);
  @$pb.TagNumber(1)
  void clearChannelType() => $_clearField(1);

  @$pb.TagNumber(2)
  $core.String get channelName => $_getSZ(1);
  @$pb.TagNumber(2)
  set channelName($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasChannelName() => $_has(1);
  @$pb.TagNumber(2)
  void clearChannelName() => $_clearField(2);

  @$pb.TagNumber(3)
  $core.String get workspace => $_getSZ(2);
  @$pb.TagNumber(3)
  set workspace($core.String value) => $_setString(2, value);
  @$pb.TagNumber(3)
  $core.bool hasWorkspace() => $_has(2);
  @$pb.TagNumber(3)
  void clearWorkspace() => $_clearField(3);
}

enum ChannelOutbound_Command { sendMessage, notSet }

class ChannelOutbound extends $pb.GeneratedMessage {
  factory ChannelOutbound({
    ChannelSend? sendMessage,
  }) {
    final result = create();
    if (sendMessage != null) result.sendMessage = sendMessage;
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
    1: ChannelOutbound_Command.sendMessage,
    0: ChannelOutbound_Command.notSet
  };
  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'ChannelOutbound',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..oo(0, [1])
    ..aOM<ChannelSend>(1, _omitFieldNames ? '' : 'sendMessage',
        subBuilder: ChannelSend.create)
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
  ChannelOutbound_Command whichCommand() =>
      _ChannelOutbound_CommandByTag[$_whichOneof(0)]!;
  @$pb.TagNumber(1)
  void clearCommand() => $_clearField($_whichOneof(0));

  @$pb.TagNumber(1)
  ChannelSend get sendMessage => $_getN(0);
  @$pb.TagNumber(1)
  set sendMessage(ChannelSend value) => $_setField(1, value);
  @$pb.TagNumber(1)
  $core.bool hasSendMessage() => $_has(0);
  @$pb.TagNumber(1)
  void clearSendMessage() => $_clearField(1);
  @$pb.TagNumber(1)
  ChannelSend ensureSendMessage() => $_ensure(0);
}

class ChannelSend extends $pb.GeneratedMessage {
  factory ChannelSend({
    $core.Iterable<ContentBlock>? content,
  }) {
    final result = create();
    if (content != null) result.content.addAll(content);
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
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..pPM<ContentBlock>(1, _omitFieldNames ? '' : 'content',
        subBuilder: ContentBlock.create)
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
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
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

/// UserMessage is the canonical "user said something" event. Flows from
/// Channel Job → Tightbeam → Transponder; reply_channel is populated by
/// Tightbeam so the transponder's response goes back to the right channel.
class UserMessage extends $pb.GeneratedMessage {
  factory UserMessage({
    $core.Iterable<ContentBlock>? content,
    $core.String? sender,
    $core.String? replyChannel,
  }) {
    final result = create();
    if (content != null) result.content.addAll(content);
    if (sender != null) result.sender = sender;
    if (replyChannel != null) result.replyChannel = replyChannel;
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
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..pPM<ContentBlock>(1, _omitFieldNames ? '' : 'content',
        subBuilder: ContentBlock.create)
    ..aOS(2, _omitFieldNames ? '' : 'sender')
    ..aOS(3, _omitFieldNames ? '' : 'replyChannel')
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
}

class EnrollRequest extends $pb.GeneratedMessage {
  factory EnrollRequest({
    $core.String? enrollmentCode,
  }) {
    final result = create();
    if (enrollmentCode != null) result.enrollmentCode = enrollmentCode;
    return result;
  }

  EnrollRequest._();

  factory EnrollRequest.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory EnrollRequest.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'EnrollRequest',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'enrollmentCode')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  EnrollRequest clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  EnrollRequest copyWith(void Function(EnrollRequest) updates) =>
      super.copyWith((message) => updates(message as EnrollRequest))
          as EnrollRequest;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static EnrollRequest create() => EnrollRequest._();
  @$core.override
  EnrollRequest createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static EnrollRequest getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<EnrollRequest>(create);
  static EnrollRequest? _defaultInstance;

  /// One-time enrollment code minted by the operator. Encoded as a
  /// signed JWT carrying {workspace, device_name, code_id, exp}.
  @$pb.TagNumber(1)
  $core.String get enrollmentCode => $_getSZ(0);
  @$pb.TagNumber(1)
  set enrollmentCode($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasEnrollmentCode() => $_has(0);
  @$pb.TagNumber(1)
  void clearEnrollmentCode() => $_clearField(1);
}

class EnrollResponse extends $pb.GeneratedMessage {
  factory EnrollResponse({
    $core.String? jwt,
    $core.String? deviceId,
    $fixnum.Int64? expiresAt,
  }) {
    final result = create();
    if (jwt != null) result.jwt = jwt;
    if (deviceId != null) result.deviceId = deviceId;
    if (expiresAt != null) result.expiresAt = expiresAt;
    return result;
  }

  EnrollResponse._();

  factory EnrollResponse.fromBuffer($core.List<$core.int> data,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromBuffer(data, registry);
  factory EnrollResponse.fromJson($core.String json,
          [$pb.ExtensionRegistry registry = $pb.ExtensionRegistry.EMPTY]) =>
      create()..mergeFromJson(json, registry);

  static final $pb.BuilderInfo _i = $pb.BuilderInfo(
      _omitMessageNames ? '' : 'EnrollResponse',
      package: const $pb.PackageName(_omitMessageNames ? '' : 'tightbeam.v1'),
      createEmptyInstance: create)
    ..aOS(1, _omitFieldNames ? '' : 'jwt')
    ..aOS(2, _omitFieldNames ? '' : 'deviceId')
    ..aInt64(3, _omitFieldNames ? '' : 'expiresAt')
    ..hasRequiredFields = false;

  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  EnrollResponse clone() => deepCopy();
  @$core.Deprecated('See https://github.com/google/protobuf.dart/issues/998.')
  EnrollResponse copyWith(void Function(EnrollResponse) updates) =>
      super.copyWith((message) => updates(message as EnrollResponse))
          as EnrollResponse;

  @$core.override
  $pb.BuilderInfo get info_ => _i;

  @$core.pragma('dart2js:noInline')
  static EnrollResponse create() => EnrollResponse._();
  @$core.override
  EnrollResponse createEmptyInstance() => create();
  @$core.pragma('dart2js:noInline')
  static EnrollResponse getDefault() => _defaultInstance ??=
      $pb.GeneratedMessage.$_defaultFor<EnrollResponse>(create);
  static EnrollResponse? _defaultInstance;

  /// Long-lived (90-day) device JWT. Client presents this as
  /// `Authorization: Bearer <jwt>` on subsequent RPCs.
  @$pb.TagNumber(1)
  $core.String get jwt => $_getSZ(0);
  @$pb.TagNumber(1)
  set jwt($core.String value) => $_setString(0, value);
  @$pb.TagNumber(1)
  $core.bool hasJwt() => $_has(0);
  @$pb.TagNumber(1)
  void clearJwt() => $_clearField(1);

  /// Server-assigned UUID for this device. Echoed back so the client can
  /// display it (operator-side revocation in a future phase will key on this).
  @$pb.TagNumber(2)
  $core.String get deviceId => $_getSZ(1);
  @$pb.TagNumber(2)
  set deviceId($core.String value) => $_setString(1, value);
  @$pb.TagNumber(2)
  $core.bool hasDeviceId() => $_has(1);
  @$pb.TagNumber(2)
  void clearDeviceId() => $_clearField(2);

  /// Unix-seconds expiry of the issued JWT. Client uses this to prompt
  /// the user for re-enrollment when expired (Phase 2 has no refresh).
  @$pb.TagNumber(3)
  $fixnum.Int64 get expiresAt => $_getI64(2);
  @$pb.TagNumber(3)
  set expiresAt($fixnum.Int64 value) => $_setInt64(2, value);
  @$pb.TagNumber(3)
  $core.bool hasExpiresAt() => $_has(2);
  @$pb.TagNumber(3)
  void clearExpiresAt() => $_clearField(3);
}

const $core.bool _omitFieldNames =
    $core.bool.fromEnvironment('protobuf.omit_field_names');
const $core.bool _omitMessageNames =
    $core.bool.fromEnvironment('protobuf.omit_message_names');
