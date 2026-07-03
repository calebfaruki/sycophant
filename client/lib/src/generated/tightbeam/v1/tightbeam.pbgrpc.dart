// This is a generated file - do not edit.
//
// Generated from tightbeam/v1/tightbeam.proto.

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
import 'tightbeam.pb.dart' as $1;

export 'tightbeam.pb.dart';

@$pb.GrpcServiceName('tightbeam.v1.TightbeamGateway')
class TightbeamGatewayClient extends $grpc.Client {
  /// The hostname for this service.
  static const $core.String defaultHost = '';

  /// OAuth scopes needed for the client.
  static const $core.List<$core.String> oauthScopes = [
    '',
  ];

  TightbeamGatewayClient(super.channel, {super.options, super.interceptors});

  $grpc.ResponseFuture<$0.RedeemEnrollmentResponse> redeemEnrollment(
    $0.RedeemEnrollmentRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$redeemEnrollment, request, options: options);
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

  $grpc.ResponseFuture<$0.CallToolResponse> callTool(
    $0.CallToolRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$callTool, request, options: options);
  }

  // method descriptors

  static final _$redeemEnrollment = $grpc.ClientMethod<
          $0.RedeemEnrollmentRequest, $0.RedeemEnrollmentResponse>(
      '/tightbeam.v1.TightbeamGateway/RedeemEnrollment',
      ($0.RedeemEnrollmentRequest value) => value.writeToBuffer(),
      $0.RedeemEnrollmentResponse.fromBuffer);
  static final _$listWorkspaces =
      $grpc.ClientMethod<$0.ListWorkspacesRequest, $0.ListWorkspacesResponse>(
          '/tightbeam.v1.TightbeamGateway/ListWorkspaces',
          ($0.ListWorkspacesRequest value) => value.writeToBuffer(),
          $0.ListWorkspacesResponse.fromBuffer);
  static final _$mintConversation = $grpc.ClientMethod<
          $0.MintConversationRequest, $0.MintConversationResponse>(
      '/tightbeam.v1.TightbeamGateway/MintConversation',
      ($0.MintConversationRequest value) => value.writeToBuffer(),
      $0.MintConversationResponse.fromBuffer);
  static final _$listConversations = $grpc.ClientMethod<
          $0.ListConversationsRequest, $0.ListConversationsResponse>(
      '/tightbeam.v1.TightbeamGateway/ListConversations',
      ($0.ListConversationsRequest value) => value.writeToBuffer(),
      $0.ListConversationsResponse.fromBuffer);
  static final _$deleteConversation = $grpc.ClientMethod<
          $0.DeleteConversationRequest, $0.DeleteConversationResponse>(
      '/tightbeam.v1.TightbeamGateway/DeleteConversation',
      ($0.DeleteConversationRequest value) => value.writeToBuffer(),
      $0.DeleteConversationResponse.fromBuffer);
  static final _$setConversationName = $grpc.ClientMethod<
          $0.SetConversationNameRequest, $0.SetConversationNameResponse>(
      '/tightbeam.v1.TightbeamGateway/SetConversationName',
      ($0.SetConversationNameRequest value) => value.writeToBuffer(),
      $0.SetConversationNameResponse.fromBuffer);
  static final _$getConversationHistory = $grpc.ClientMethod<
          $0.GetConversationHistoryRequest, $0.GetConversationHistoryResponse>(
      '/tightbeam.v1.TightbeamGateway/GetConversationHistory',
      ($0.GetConversationHistoryRequest value) => value.writeToBuffer(),
      $0.GetConversationHistoryResponse.fromBuffer);
  static final _$getTurnState =
      $grpc.ClientMethod<$0.GetTurnStateRequest, $0.TurnStateEvent>(
          '/tightbeam.v1.TightbeamGateway/GetTurnState',
          ($0.GetTurnStateRequest value) => value.writeToBuffer(),
          $0.TurnStateEvent.fromBuffer);
  static final _$channelIngest =
      $grpc.ClientMethod<$0.ChannelIngestRequest, $0.ChannelIngestAck>(
          '/tightbeam.v1.TightbeamGateway/ChannelIngest',
          ($0.ChannelIngestRequest value) => value.writeToBuffer(),
          $0.ChannelIngestAck.fromBuffer);
  static final _$channelReceive =
      $grpc.ClientMethod<$0.ChannelReceiveRequest, $0.ChannelOutbound>(
          '/tightbeam.v1.TightbeamGateway/ChannelReceive',
          ($0.ChannelReceiveRequest value) => value.writeToBuffer(),
          $0.ChannelOutbound.fromBuffer);
  static final _$watchTools =
      $grpc.ClientMethod<$0.WatchToolsRequest, $0.ToolListUpdate>(
          '/tightbeam.v1.TightbeamGateway/WatchTools',
          ($0.WatchToolsRequest value) => value.writeToBuffer(),
          $0.ToolListUpdate.fromBuffer);
  static final _$callTool =
      $grpc.ClientMethod<$0.CallToolRequest, $0.CallToolResponse>(
          '/tightbeam.v1.TightbeamGateway/CallTool',
          ($0.CallToolRequest value) => value.writeToBuffer(),
          $0.CallToolResponse.fromBuffer);
}

@$pb.GrpcServiceName('tightbeam.v1.TightbeamGateway')
abstract class TightbeamGatewayServiceBase extends $grpc.Service {
  $core.String get $name => 'tightbeam.v1.TightbeamGateway';

  TightbeamGatewayServiceBase() {
    $addMethod($grpc.ServiceMethod<$0.RedeemEnrollmentRequest,
            $0.RedeemEnrollmentResponse>(
        'RedeemEnrollment',
        redeemEnrollment_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.RedeemEnrollmentRequest.fromBuffer(value),
        ($0.RedeemEnrollmentResponse value) => value.writeToBuffer()));
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
    $addMethod($grpc.ServiceMethod<$0.CallToolRequest, $0.CallToolResponse>(
        'CallTool',
        callTool_Pre,
        false,
        false,
        ($core.List<$core.int> value) => $0.CallToolRequest.fromBuffer(value),
        ($0.CallToolResponse value) => value.writeToBuffer()));
  }

  $async.Future<$0.RedeemEnrollmentResponse> redeemEnrollment_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.RedeemEnrollmentRequest> $request) async {
    return redeemEnrollment($call, await $request);
  }

  $async.Future<$0.RedeemEnrollmentResponse> redeemEnrollment(
      $grpc.ServiceCall call, $0.RedeemEnrollmentRequest request);

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

  $async.Future<$0.CallToolResponse> callTool_Pre($grpc.ServiceCall $call,
      $async.Future<$0.CallToolRequest> $request) async {
    return callTool($call, await $request);
  }

  $async.Future<$0.CallToolResponse> callTool(
      $grpc.ServiceCall call, $0.CallToolRequest request);
}

@$pb.GrpcServiceName('tightbeam.v1.TightbeamInternal')
class TightbeamInternalClient extends $grpc.Client {
  /// The hostname for this service.
  static const $core.String defaultHost = '';

  /// OAuth scopes needed for the client.
  static const $core.List<$core.String> oauthScopes = [
    '',
  ];

  TightbeamInternalClient(super.channel, {super.options, super.interceptors});

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

  /// hangar (Stage 4: transponder) pushes assistant reply + terminal
  /// turn-state in one ordered call.
  $grpc.ResponseFuture<$1.DeliverOutboundResponse> deliverOutbound(
    $1.DeliverOutboundRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$deliverOutbound, request, options: options);
  }

  // method descriptors

  static final _$subscribe =
      $grpc.ClientMethod<$0.SubscribeRequest, $0.UserMessage>(
          '/tightbeam.v1.TightbeamInternal/Subscribe',
          ($0.SubscribeRequest value) => value.writeToBuffer(),
          $0.UserMessage.fromBuffer);
  static final _$sendServerNotification = $grpc.ClientMethod<
          $0.SendServerNotificationRequest, $0.SendServerNotificationResponse>(
      '/tightbeam.v1.TightbeamInternal/SendServerNotification',
      ($0.SendServerNotificationRequest value) => value.writeToBuffer(),
      $0.SendServerNotificationResponse.fromBuffer);
  static final _$sendServerRequestAndAwait = $grpc.ClientMethod<
          $0.SendServerRequestAndAwaitRequest,
          $0.SendServerRequestAndAwaitResponse>(
      '/tightbeam.v1.TightbeamInternal/SendServerRequestAndAwait',
      ($0.SendServerRequestAndAwaitRequest value) => value.writeToBuffer(),
      $0.SendServerRequestAndAwaitResponse.fromBuffer);
  static final _$deliverOutbound =
      $grpc.ClientMethod<$1.DeliverOutboundRequest, $1.DeliverOutboundResponse>(
          '/tightbeam.v1.TightbeamInternal/DeliverOutbound',
          ($1.DeliverOutboundRequest value) => value.writeToBuffer(),
          $1.DeliverOutboundResponse.fromBuffer);
}

@$pb.GrpcServiceName('tightbeam.v1.TightbeamInternal')
abstract class TightbeamInternalServiceBase extends $grpc.Service {
  $core.String get $name => 'tightbeam.v1.TightbeamInternal';

  TightbeamInternalServiceBase() {
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
}
