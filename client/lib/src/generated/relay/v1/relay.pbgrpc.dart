// This is a generated file - do not edit.
//
// Generated from relay/v1/relay.proto.

// @dart = 3.3

// ignore_for_file: annotate_overrides, camel_case_types, comment_references
// ignore_for_file: constant_identifier_names
// ignore_for_file: curly_braces_in_flow_control_structures
// ignore_for_file: deprecated_member_use_from_same_package, library_prefixes
// ignore_for_file: non_constant_identifier_names, prefer_relative_imports

import 'dart:async' as $async;
import 'dart:core' as $core;

import 'package:grpc/service_api.dart' as $grpc;
import 'package:protobuf/protobuf.dart' as $pb;

import '../../sycophant/common/v1/common.pb.dart' as $0;
import 'relay.pb.dart' as $1;

export 'relay.pb.dart';

@$pb.GrpcServiceName('relay.v1.RelayGateway')
class RelayGatewayClient extends $grpc.Client {
  /// The hostname for this service.
  static const $core.String defaultHost = '';

  /// OAuth scopes needed for the client.
  static const $core.List<$core.String> oauthScopes = [
    '',
  ];

  RelayGatewayClient(super.channel, {super.options, super.interceptors});

  $grpc.ResponseFuture<$0.RedeemCodeResponse> redeemCode(
    $0.RedeemCodeRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$redeemCode, request, options: options);
  }

  $grpc.ResponseFuture<$0.ListWorkspacesResponse> listWorkspaces(
    $0.ListWorkspacesRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$listWorkspaces, request, options: options);
  }

  $grpc.ResponseFuture<$0.MintConversationResponse> mintConversation(
    $0.MintConversationRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$mintConversation, request, options: options);
  }

  $grpc.ResponseFuture<$0.ListConversationsResponse> listConversations(
    $0.ListConversationsRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$listConversations, request, options: options);
  }

  $grpc.ResponseFuture<$0.DeleteConversationResponse> deleteConversation(
    $0.DeleteConversationRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$deleteConversation, request, options: options);
  }

  $grpc.ResponseFuture<$0.SetConversationNameResponse> setConversationName(
    $0.SetConversationNameRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$setConversationName, request, options: options);
  }

  $grpc.ResponseFuture<$0.GetConversationHistoryResponse>
      getConversationHistory(
    $0.GetConversationHistoryRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$getConversationHistory, request,
        options: options);
  }

  $grpc.ResponseFuture<$0.TurnStateEvent> getTurnState(
    $0.GetTurnStateRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$getTurnState, request, options: options);
  }

  $grpc.ResponseFuture<$0.CancelTurnResponse> cancelTurn(
    $0.CancelTurnRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$cancelTurn, request, options: options);
  }

  $grpc.ResponseFuture<$0.ChannelIngestAck> channelIngest(
    $0.ChannelIngestRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$channelIngest, request, options: options);
  }

  $grpc.ResponseStream<$0.ChannelOutbound> channelReceive(
    $0.ChannelReceiveRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(
        _$channelReceive, $async.Stream.fromIterable([request]),
        options: options);
  }

  $grpc.ResponseStream<$0.ToolListUpdate> watchTools(
    $0.WatchToolsRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(
        _$watchTools, $async.Stream.fromIterable([request]),
        options: options);
  }

  /// --- Client-driven tool-call lifecycle (dispatch / await / cancel) ---
  ///
  /// Verify-then-forward the harness's cancelable tool-call surface to the
  /// caller's verified workspace. Same request/response types as the harness.
  $grpc.ResponseFuture<$0.DispatchToolResponse> dispatchTool(
    $0.CallToolRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$dispatchTool, request, options: options);
  }

  $grpc.ResponseStream<$0.ToolResultFrame> awaitToolResult(
    $0.AwaitToolResultRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(
        _$awaitToolResult, $async.Stream.fromIterable([request]),
        options: options);
  }

  $grpc.ResponseFuture<$0.CancelToolResponse> cancelTool(
    $0.CancelToolRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$cancelTool, request, options: options);
  }

  // method descriptors

  static final _$redeemCode =
      $grpc.ClientMethod<$0.RedeemCodeRequest, $0.RedeemCodeResponse>(
          '/relay.v1.RelayGateway/RedeemCode',
          ($0.RedeemCodeRequest value) => value.writeToBuffer(),
          $0.RedeemCodeResponse.fromBuffer);
  static final _$listWorkspaces =
      $grpc.ClientMethod<$0.ListWorkspacesRequest, $0.ListWorkspacesResponse>(
          '/relay.v1.RelayGateway/ListWorkspaces',
          ($0.ListWorkspacesRequest value) => value.writeToBuffer(),
          $0.ListWorkspacesResponse.fromBuffer);
  static final _$mintConversation = $grpc.ClientMethod<
          $0.MintConversationRequest, $0.MintConversationResponse>(
      '/relay.v1.RelayGateway/MintConversation',
      ($0.MintConversationRequest value) => value.writeToBuffer(),
      $0.MintConversationResponse.fromBuffer);
  static final _$listConversations = $grpc.ClientMethod<
          $0.ListConversationsRequest, $0.ListConversationsResponse>(
      '/relay.v1.RelayGateway/ListConversations',
      ($0.ListConversationsRequest value) => value.writeToBuffer(),
      $0.ListConversationsResponse.fromBuffer);
  static final _$deleteConversation = $grpc.ClientMethod<
          $0.DeleteConversationRequest, $0.DeleteConversationResponse>(
      '/relay.v1.RelayGateway/DeleteConversation',
      ($0.DeleteConversationRequest value) => value.writeToBuffer(),
      $0.DeleteConversationResponse.fromBuffer);
  static final _$setConversationName = $grpc.ClientMethod<
          $0.SetConversationNameRequest, $0.SetConversationNameResponse>(
      '/relay.v1.RelayGateway/SetConversationName',
      ($0.SetConversationNameRequest value) => value.writeToBuffer(),
      $0.SetConversationNameResponse.fromBuffer);
  static final _$getConversationHistory = $grpc.ClientMethod<
          $0.GetConversationHistoryRequest, $0.GetConversationHistoryResponse>(
      '/relay.v1.RelayGateway/GetConversationHistory',
      ($0.GetConversationHistoryRequest value) => value.writeToBuffer(),
      $0.GetConversationHistoryResponse.fromBuffer);
  static final _$getTurnState =
      $grpc.ClientMethod<$0.GetTurnStateRequest, $0.TurnStateEvent>(
          '/relay.v1.RelayGateway/GetTurnState',
          ($0.GetTurnStateRequest value) => value.writeToBuffer(),
          $0.TurnStateEvent.fromBuffer);
  static final _$cancelTurn =
      $grpc.ClientMethod<$0.CancelTurnRequest, $0.CancelTurnResponse>(
          '/relay.v1.RelayGateway/CancelTurn',
          ($0.CancelTurnRequest value) => value.writeToBuffer(),
          $0.CancelTurnResponse.fromBuffer);
  static final _$channelIngest =
      $grpc.ClientMethod<$0.ChannelIngestRequest, $0.ChannelIngestAck>(
          '/relay.v1.RelayGateway/ChannelIngest',
          ($0.ChannelIngestRequest value) => value.writeToBuffer(),
          $0.ChannelIngestAck.fromBuffer);
  static final _$channelReceive =
      $grpc.ClientMethod<$0.ChannelReceiveRequest, $0.ChannelOutbound>(
          '/relay.v1.RelayGateway/ChannelReceive',
          ($0.ChannelReceiveRequest value) => value.writeToBuffer(),
          $0.ChannelOutbound.fromBuffer);
  static final _$watchTools =
      $grpc.ClientMethod<$0.WatchToolsRequest, $0.ToolListUpdate>(
          '/relay.v1.RelayGateway/WatchTools',
          ($0.WatchToolsRequest value) => value.writeToBuffer(),
          $0.ToolListUpdate.fromBuffer);
  static final _$dispatchTool =
      $grpc.ClientMethod<$0.CallToolRequest, $0.DispatchToolResponse>(
          '/relay.v1.RelayGateway/DispatchTool',
          ($0.CallToolRequest value) => value.writeToBuffer(),
          $0.DispatchToolResponse.fromBuffer);
  static final _$awaitToolResult =
      $grpc.ClientMethod<$0.AwaitToolResultRequest, $0.ToolResultFrame>(
          '/relay.v1.RelayGateway/AwaitToolResult',
          ($0.AwaitToolResultRequest value) => value.writeToBuffer(),
          $0.ToolResultFrame.fromBuffer);
  static final _$cancelTool =
      $grpc.ClientMethod<$0.CancelToolRequest, $0.CancelToolResponse>(
          '/relay.v1.RelayGateway/CancelTool',
          ($0.CancelToolRequest value) => value.writeToBuffer(),
          $0.CancelToolResponse.fromBuffer);
}

@$pb.GrpcServiceName('relay.v1.RelayGateway')
abstract class RelayGatewayServiceBase extends $grpc.Service {
  $core.String get $name => 'relay.v1.RelayGateway';

  RelayGatewayServiceBase() {
    $addMethod($grpc.ServiceMethod<$0.RedeemCodeRequest, $0.RedeemCodeResponse>(
        'RedeemCode',
        redeemCode_Pre,
        false,
        false,
        ($core.List<$core.int> value) => $0.RedeemCodeRequest.fromBuffer(value),
        ($0.RedeemCodeResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.ListWorkspacesRequest,
            $0.ListWorkspacesResponse>(
        'ListWorkspaces',
        listWorkspaces_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.ListWorkspacesRequest.fromBuffer(value),
        ($0.ListWorkspacesResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.MintConversationRequest,
            $0.MintConversationResponse>(
        'MintConversation',
        mintConversation_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.MintConversationRequest.fromBuffer(value),
        ($0.MintConversationResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.ListConversationsRequest,
            $0.ListConversationsResponse>(
        'ListConversations',
        listConversations_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.ListConversationsRequest.fromBuffer(value),
        ($0.ListConversationsResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.DeleteConversationRequest,
            $0.DeleteConversationResponse>(
        'DeleteConversation',
        deleteConversation_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.DeleteConversationRequest.fromBuffer(value),
        ($0.DeleteConversationResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.SetConversationNameRequest,
            $0.SetConversationNameResponse>(
        'SetConversationName',
        setConversationName_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.SetConversationNameRequest.fromBuffer(value),
        ($0.SetConversationNameResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.GetConversationHistoryRequest,
            $0.GetConversationHistoryResponse>(
        'GetConversationHistory',
        getConversationHistory_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.GetConversationHistoryRequest.fromBuffer(value),
        ($0.GetConversationHistoryResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.GetTurnStateRequest, $0.TurnStateEvent>(
        'GetTurnState',
        getTurnState_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.GetTurnStateRequest.fromBuffer(value),
        ($0.TurnStateEvent value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.CancelTurnRequest, $0.CancelTurnResponse>(
        'CancelTurn',
        cancelTurn_Pre,
        false,
        false,
        ($core.List<$core.int> value) => $0.CancelTurnRequest.fromBuffer(value),
        ($0.CancelTurnResponse value) => value.writeToBuffer()));
    $addMethod(
        $grpc.ServiceMethod<$0.ChannelIngestRequest, $0.ChannelIngestAck>(
            'ChannelIngest',
            channelIngest_Pre,
            false,
            false,
            ($core.List<$core.int> value) =>
                $0.ChannelIngestRequest.fromBuffer(value),
            ($0.ChannelIngestAck value) => value.writeToBuffer()));
    $addMethod(
        $grpc.ServiceMethod<$0.ChannelReceiveRequest, $0.ChannelOutbound>(
            'ChannelReceive',
            channelReceive_Pre,
            false,
            true,
            ($core.List<$core.int> value) =>
                $0.ChannelReceiveRequest.fromBuffer(value),
            ($0.ChannelOutbound value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.WatchToolsRequest, $0.ToolListUpdate>(
        'WatchTools',
        watchTools_Pre,
        false,
        true,
        ($core.List<$core.int> value) => $0.WatchToolsRequest.fromBuffer(value),
        ($0.ToolListUpdate value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.CallToolRequest, $0.DispatchToolResponse>(
        'DispatchTool',
        dispatchTool_Pre,
        false,
        false,
        ($core.List<$core.int> value) => $0.CallToolRequest.fromBuffer(value),
        ($0.DispatchToolResponse value) => value.writeToBuffer()));
    $addMethod(
        $grpc.ServiceMethod<$0.AwaitToolResultRequest, $0.ToolResultFrame>(
            'AwaitToolResult',
            awaitToolResult_Pre,
            false,
            true,
            ($core.List<$core.int> value) =>
                $0.AwaitToolResultRequest.fromBuffer(value),
            ($0.ToolResultFrame value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.CancelToolRequest, $0.CancelToolResponse>(
        'CancelTool',
        cancelTool_Pre,
        false,
        false,
        ($core.List<$core.int> value) => $0.CancelToolRequest.fromBuffer(value),
        ($0.CancelToolResponse value) => value.writeToBuffer()));
  }

  $async.Future<$0.RedeemCodeResponse> redeemCode_Pre($grpc.ServiceCall $call,
      $async.Future<$0.RedeemCodeRequest> $request) async {
    return redeemCode($call, await $request);
  }

  $async.Future<$0.RedeemCodeResponse> redeemCode(
      $grpc.ServiceCall call, $0.RedeemCodeRequest request);

  $async.Future<$0.ListWorkspacesResponse> listWorkspaces_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.ListWorkspacesRequest> $request) async {
    return listWorkspaces($call, await $request);
  }

  $async.Future<$0.ListWorkspacesResponse> listWorkspaces(
      $grpc.ServiceCall call, $0.ListWorkspacesRequest request);

  $async.Future<$0.MintConversationResponse> mintConversation_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.MintConversationRequest> $request) async {
    return mintConversation($call, await $request);
  }

  $async.Future<$0.MintConversationResponse> mintConversation(
      $grpc.ServiceCall call, $0.MintConversationRequest request);

  $async.Future<$0.ListConversationsResponse> listConversations_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.ListConversationsRequest> $request) async {
    return listConversations($call, await $request);
  }

  $async.Future<$0.ListConversationsResponse> listConversations(
      $grpc.ServiceCall call, $0.ListConversationsRequest request);

  $async.Future<$0.DeleteConversationResponse> deleteConversation_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.DeleteConversationRequest> $request) async {
    return deleteConversation($call, await $request);
  }

  $async.Future<$0.DeleteConversationResponse> deleteConversation(
      $grpc.ServiceCall call, $0.DeleteConversationRequest request);

  $async.Future<$0.SetConversationNameResponse> setConversationName_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.SetConversationNameRequest> $request) async {
    return setConversationName($call, await $request);
  }

  $async.Future<$0.SetConversationNameResponse> setConversationName(
      $grpc.ServiceCall call, $0.SetConversationNameRequest request);

  $async.Future<$0.GetConversationHistoryResponse> getConversationHistory_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.GetConversationHistoryRequest> $request) async {
    return getConversationHistory($call, await $request);
  }

  $async.Future<$0.GetConversationHistoryResponse> getConversationHistory(
      $grpc.ServiceCall call, $0.GetConversationHistoryRequest request);

  $async.Future<$0.TurnStateEvent> getTurnState_Pre($grpc.ServiceCall $call,
      $async.Future<$0.GetTurnStateRequest> $request) async {
    return getTurnState($call, await $request);
  }

  $async.Future<$0.TurnStateEvent> getTurnState(
      $grpc.ServiceCall call, $0.GetTurnStateRequest request);

  $async.Future<$0.CancelTurnResponse> cancelTurn_Pre($grpc.ServiceCall $call,
      $async.Future<$0.CancelTurnRequest> $request) async {
    return cancelTurn($call, await $request);
  }

  $async.Future<$0.CancelTurnResponse> cancelTurn(
      $grpc.ServiceCall call, $0.CancelTurnRequest request);

  $async.Future<$0.ChannelIngestAck> channelIngest_Pre($grpc.ServiceCall $call,
      $async.Future<$0.ChannelIngestRequest> $request) async {
    return channelIngest($call, await $request);
  }

  $async.Future<$0.ChannelIngestAck> channelIngest(
      $grpc.ServiceCall call, $0.ChannelIngestRequest request);

  $async.Stream<$0.ChannelOutbound> channelReceive_Pre($grpc.ServiceCall $call,
      $async.Future<$0.ChannelReceiveRequest> $request) async* {
    yield* channelReceive($call, await $request);
  }

  $async.Stream<$0.ChannelOutbound> channelReceive(
      $grpc.ServiceCall call, $0.ChannelReceiveRequest request);

  $async.Stream<$0.ToolListUpdate> watchTools_Pre($grpc.ServiceCall $call,
      $async.Future<$0.WatchToolsRequest> $request) async* {
    yield* watchTools($call, await $request);
  }

  $async.Stream<$0.ToolListUpdate> watchTools(
      $grpc.ServiceCall call, $0.WatchToolsRequest request);

  $async.Future<$0.DispatchToolResponse> dispatchTool_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.CallToolRequest> $request) async {
    return dispatchTool($call, await $request);
  }

  $async.Future<$0.DispatchToolResponse> dispatchTool(
      $grpc.ServiceCall call, $0.CallToolRequest request);

  $async.Stream<$0.ToolResultFrame> awaitToolResult_Pre($grpc.ServiceCall $call,
      $async.Future<$0.AwaitToolResultRequest> $request) async* {
    yield* awaitToolResult($call, await $request);
  }

  $async.Stream<$0.ToolResultFrame> awaitToolResult(
      $grpc.ServiceCall call, $0.AwaitToolResultRequest request);

  $async.Future<$0.CancelToolResponse> cancelTool_Pre($grpc.ServiceCall $call,
      $async.Future<$0.CancelToolRequest> $request) async {
    return cancelTool($call, await $request);
  }

  $async.Future<$0.CancelToolResponse> cancelTool(
      $grpc.ServiceCall call, $0.CancelToolRequest request);
}

@$pb.GrpcServiceName('relay.v1.RelayInternal')
class RelayInternalClient extends $grpc.Client {
  /// The hostname for this service.
  static const $core.String defaultHost = '';

  /// OAuth scopes needed for the client.
  static const $core.List<$core.String> oauthScopes = [
    '',
  ];

  RelayInternalClient(super.channel, {super.options, super.interceptors});

  $grpc.ResponseStream<$0.UserMessage> subscribe(
    $0.SubscribeRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(
        _$subscribe, $async.Stream.fromIterable([request]),
        options: options);
  }

  $grpc.ResponseFuture<$0.SendServerNotificationResponse>
      sendServerNotification(
    $0.SendServerNotificationRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$sendServerNotification, request,
        options: options);
  }

  $grpc.ResponseFuture<$0.SendServerRequestAndAwaitResponse>
      sendServerRequestAndAwait(
    $0.SendServerRequestAndAwaitRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$sendServerRequestAndAwait, request,
        options: options);
  }

  /// The harness pushes assistant reply + terminal turn-state in one
  /// ordered call.
  $grpc.ResponseFuture<$1.DeliverOutboundResponse> deliverOutbound(
    $1.DeliverOutboundRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$deliverOutbound, request, options: options);
  }

  /// Harness pushes one streamed activity frame produced during a turn.
  /// Unary-per-frame; the gateway wraps the StreamItem verbatim into a
  /// ChannelOutbound and relays it unchanged (no payload inspection).
  $grpc.ResponseFuture<$1.DeliverStreamItemResponse> deliverStreamItem(
    $1.DeliverStreamItemRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$deliverStreamItem, request, options: options);
  }

  // method descriptors

  static final _$subscribe =
      $grpc.ClientMethod<$0.SubscribeRequest, $0.UserMessage>(
          '/relay.v1.RelayInternal/Subscribe',
          ($0.SubscribeRequest value) => value.writeToBuffer(),
          $0.UserMessage.fromBuffer);
  static final _$sendServerNotification = $grpc.ClientMethod<
          $0.SendServerNotificationRequest, $0.SendServerNotificationResponse>(
      '/relay.v1.RelayInternal/SendServerNotification',
      ($0.SendServerNotificationRequest value) => value.writeToBuffer(),
      $0.SendServerNotificationResponse.fromBuffer);
  static final _$sendServerRequestAndAwait = $grpc.ClientMethod<
          $0.SendServerRequestAndAwaitRequest,
          $0.SendServerRequestAndAwaitResponse>(
      '/relay.v1.RelayInternal/SendServerRequestAndAwait',
      ($0.SendServerRequestAndAwaitRequest value) => value.writeToBuffer(),
      $0.SendServerRequestAndAwaitResponse.fromBuffer);
  static final _$deliverOutbound =
      $grpc.ClientMethod<$1.DeliverOutboundRequest, $1.DeliverOutboundResponse>(
          '/relay.v1.RelayInternal/DeliverOutbound',
          ($1.DeliverOutboundRequest value) => value.writeToBuffer(),
          $1.DeliverOutboundResponse.fromBuffer);
  static final _$deliverStreamItem = $grpc.ClientMethod<
          $1.DeliverStreamItemRequest, $1.DeliverStreamItemResponse>(
      '/relay.v1.RelayInternal/DeliverStreamItem',
      ($1.DeliverStreamItemRequest value) => value.writeToBuffer(),
      $1.DeliverStreamItemResponse.fromBuffer);
}

@$pb.GrpcServiceName('relay.v1.RelayInternal')
abstract class RelayInternalServiceBase extends $grpc.Service {
  $core.String get $name => 'relay.v1.RelayInternal';

  RelayInternalServiceBase() {
    $addMethod($grpc.ServiceMethod<$0.SubscribeRequest, $0.UserMessage>(
        'Subscribe',
        subscribe_Pre,
        false,
        true,
        ($core.List<$core.int> value) => $0.SubscribeRequest.fromBuffer(value),
        ($0.UserMessage value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.SendServerNotificationRequest,
            $0.SendServerNotificationResponse>(
        'SendServerNotification',
        sendServerNotification_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.SendServerNotificationRequest.fromBuffer(value),
        ($0.SendServerNotificationResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.SendServerRequestAndAwaitRequest,
            $0.SendServerRequestAndAwaitResponse>(
        'SendServerRequestAndAwait',
        sendServerRequestAndAwait_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.SendServerRequestAndAwaitRequest.fromBuffer(value),
        ($0.SendServerRequestAndAwaitResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$1.DeliverOutboundRequest,
            $1.DeliverOutboundResponse>(
        'DeliverOutbound',
        deliverOutbound_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $1.DeliverOutboundRequest.fromBuffer(value),
        ($1.DeliverOutboundResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$1.DeliverStreamItemRequest,
            $1.DeliverStreamItemResponse>(
        'DeliverStreamItem',
        deliverStreamItem_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $1.DeliverStreamItemRequest.fromBuffer(value),
        ($1.DeliverStreamItemResponse value) => value.writeToBuffer()));
  }

  $async.Stream<$0.UserMessage> subscribe_Pre($grpc.ServiceCall $call,
      $async.Future<$0.SubscribeRequest> $request) async* {
    yield* subscribe($call, await $request);
  }

  $async.Stream<$0.UserMessage> subscribe(
      $grpc.ServiceCall call, $0.SubscribeRequest request);

  $async.Future<$0.SendServerNotificationResponse> sendServerNotification_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.SendServerNotificationRequest> $request) async {
    return sendServerNotification($call, await $request);
  }

  $async.Future<$0.SendServerNotificationResponse> sendServerNotification(
      $grpc.ServiceCall call, $0.SendServerNotificationRequest request);

  $async.Future<$0.SendServerRequestAndAwaitResponse>
      sendServerRequestAndAwait_Pre($grpc.ServiceCall $call,
          $async.Future<$0.SendServerRequestAndAwaitRequest> $request) async {
    return sendServerRequestAndAwait($call, await $request);
  }

  $async.Future<$0.SendServerRequestAndAwaitResponse> sendServerRequestAndAwait(
      $grpc.ServiceCall call, $0.SendServerRequestAndAwaitRequest request);

  $async.Future<$1.DeliverOutboundResponse> deliverOutbound_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$1.DeliverOutboundRequest> $request) async {
    return deliverOutbound($call, await $request);
  }

  $async.Future<$1.DeliverOutboundResponse> deliverOutbound(
      $grpc.ServiceCall call, $1.DeliverOutboundRequest request);

  $async.Future<$1.DeliverStreamItemResponse> deliverStreamItem_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$1.DeliverStreamItemRequest> $request) async {
    return deliverStreamItem($call, await $request);
  }

  $async.Future<$1.DeliverStreamItemResponse> deliverStreamItem(
      $grpc.ServiceCall call, $1.DeliverStreamItemRequest request);
}
