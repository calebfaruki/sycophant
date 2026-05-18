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

import 'tightbeam.pb.dart' as $0;

export 'tightbeam.pb.dart';

@$pb.GrpcServiceName('tightbeam.v1.TightbeamController')
class TightbeamControllerClient extends $grpc.Client {
  /// The hostname for this service.
  static const $core.String defaultHost = '';

  /// OAuth scopes needed for the client.
  static const $core.List<$core.String> oauthScopes = [
    '',
  ];

  TightbeamControllerClient(super.channel, {super.options, super.interceptors});

  /// LLM Job pulls work. Long-poll: controller holds the response open
  /// until a turn is ready. The Job sets a gRPC deadline as its idle
  /// timeout. No work before deadline = Job exits, kubelet cleans up.
  $grpc.ResponseFuture<$0.TurnAssignment> getTurn(
    $0.GetTurnRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$getTurn, request, options: options);
  }

  /// LLM Job streams response chunks back to the controller.
  $grpc.ResponseFuture<$0.TurnAck> streamTurnResult(
    $async.Stream<$0.TurnResultChunk> request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(_$streamTurnResult, request, options: options)
        .single;
  }

  /// Transponder sends turns, receives streaming LLM response.
  $grpc.ResponseStream<$0.TurnEvent> turn(
    $0.TurnRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(_$turn, $async.Stream.fromIterable([request]),
        options: options);
  }

  /// Mint a fresh conversation id. The controller is the only party that
  /// mints ids; callers thread the returned id through follow-up messages.
  $grpc.ResponseFuture<$0.MintConversationResponse> mintConversation(
    $0.MintConversationRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$mintConversation, request, options: options);
  }

  /// List conversation ids known to a workspace. Returns ids that have any
  /// persisted events. Used by dashboards rendering a chat-thread sidebar.
  $grpc.ResponseFuture<$0.ListConversationsResponse> listConversations(
    $0.ListConversationsRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$listConversations, request, options: options);
  }

  /// Channel Job bidirectional stream. Inbound user messages flow in,
  /// agent responses flow out.
  $grpc.ResponseStream<$0.ChannelOutbound> channelStream(
    $async.Stream<$0.ChannelInbound> request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(_$channelStream, request, options: options);
  }

  /// Transponder subscribes to receive inbound human messages.
  /// Server-streaming: controller pushes messages as they arrive from channels.
  $grpc.ResponseStream<$0.UserMessage> subscribe(
    $0.SubscribeRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(
        _$subscribe, $async.Stream.fromIterable([request]),
        options: options);
  }

  /// Client enrollment redemption. External client presents a one-time
  /// enrollment code minted by the controller (status.enrollmentCode on
  /// the matching Client CR) and its freshly-generated P-256 public key.
  /// Controller validates the code's signature + expiry + claims,
  /// persists the public key on the Client CR's status.publicKey, and
  /// clears the enrollmentCode. Subsequent requests sign each call with
  /// the client's private key (verified by ClientSignatureVerifier on
  /// the external listener). RedeemEnrollment itself is unauthenticated
  /// by design — the enrollment code is the authentication artifact.
  $grpc.ResponseFuture<$0.RedeemEnrollmentResponse> redeemEnrollment(
    $0.RedeemEnrollmentRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$redeemEnrollment, request, options: options);
  }

  /// Read the tail of a conversation's history. Backs the transponder's
  /// built-in `recent_turns` tool, replacing the workspace pod's prior
  /// filesystem mount of the conversation log. Workspace is derived
  /// from the calling SA token; the conversation_id must belong to that
  /// workspace.
  $grpc.ResponseFuture<$0.GetConversationHistoryResponse>
      getConversationHistory(
    $0.GetConversationHistoryRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$getConversationHistory, request,
        options: options);
  }

  /// External user-message ingress. The caller (Flutter app, future SPA)
  /// acts as a channel adapter for a single end-user; this is the unary
  /// equivalent of one ChannelInbound::UserMessage frame on ChannelStream.
  /// The controller routes the message to the workspace transponder's
  /// Subscribe stream, where the agent loop builds the actual TurnRequest
  /// from AGENTS.md + the workspace's tool catalog. This is the ONLY
  /// external user-input path to the agent — Turn is internal-only
  /// because the transponder is the sole authority for what gets
  /// dispatched to the LLM for a workspace.
  ///
  /// Contract: the caller MUST have a ChannelReceive stream open with
  /// the matching (channel_type, channel_name) before invoking
  /// ChannelIngest, or the agent's reply has no destination.
  $grpc.ResponseFuture<$0.ChannelIngestAck> channelIngest(
    $0.ChannelIngestRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$channelIngest, request, options: options);
  }

  /// External outbound-message reception. The Flutter app opens this
  /// server-stream once per chat session keyed by (channel_type,
  /// channel_name) and receives the agent's responses as ChannelOutbound
  /// events. Workspace is derived from the calling client's signature.
  /// Server-streaming is compatible with the signature middleware: only
  /// streaming REQUESTS deadlock the body-collect path; streaming
  /// RESPONSES are fine because the request body is bounded and hashed
  /// pre-dispatch.
  $grpc.ResponseStream<$0.ChannelOutbound> channelReceive(
    $0.ChannelReceiveRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(
        _$channelReceive, $async.Stream.fromIterable([request]),
        options: options);
  }

  // method descriptors

  static final _$getTurn =
      $grpc.ClientMethod<$0.GetTurnRequest, $0.TurnAssignment>(
          '/tightbeam.v1.TightbeamController/GetTurn',
          ($0.GetTurnRequest value) => value.writeToBuffer(),
          $0.TurnAssignment.fromBuffer);
  static final _$streamTurnResult =
      $grpc.ClientMethod<$0.TurnResultChunk, $0.TurnAck>(
          '/tightbeam.v1.TightbeamController/StreamTurnResult',
          ($0.TurnResultChunk value) => value.writeToBuffer(),
          $0.TurnAck.fromBuffer);
  static final _$turn = $grpc.ClientMethod<$0.TurnRequest, $0.TurnEvent>(
      '/tightbeam.v1.TightbeamController/Turn',
      ($0.TurnRequest value) => value.writeToBuffer(),
      $0.TurnEvent.fromBuffer);
  static final _$mintConversation = $grpc.ClientMethod<
          $0.MintConversationRequest, $0.MintConversationResponse>(
      '/tightbeam.v1.TightbeamController/MintConversation',
      ($0.MintConversationRequest value) => value.writeToBuffer(),
      $0.MintConversationResponse.fromBuffer);
  static final _$listConversations = $grpc.ClientMethod<
          $0.ListConversationsRequest, $0.ListConversationsResponse>(
      '/tightbeam.v1.TightbeamController/ListConversations',
      ($0.ListConversationsRequest value) => value.writeToBuffer(),
      $0.ListConversationsResponse.fromBuffer);
  static final _$channelStream =
      $grpc.ClientMethod<$0.ChannelInbound, $0.ChannelOutbound>(
          '/tightbeam.v1.TightbeamController/ChannelStream',
          ($0.ChannelInbound value) => value.writeToBuffer(),
          $0.ChannelOutbound.fromBuffer);
  static final _$subscribe =
      $grpc.ClientMethod<$0.SubscribeRequest, $0.UserMessage>(
          '/tightbeam.v1.TightbeamController/Subscribe',
          ($0.SubscribeRequest value) => value.writeToBuffer(),
          $0.UserMessage.fromBuffer);
  static final _$redeemEnrollment = $grpc.ClientMethod<
          $0.RedeemEnrollmentRequest, $0.RedeemEnrollmentResponse>(
      '/tightbeam.v1.TightbeamController/RedeemEnrollment',
      ($0.RedeemEnrollmentRequest value) => value.writeToBuffer(),
      $0.RedeemEnrollmentResponse.fromBuffer);
  static final _$getConversationHistory = $grpc.ClientMethod<
          $0.GetConversationHistoryRequest, $0.GetConversationHistoryResponse>(
      '/tightbeam.v1.TightbeamController/GetConversationHistory',
      ($0.GetConversationHistoryRequest value) => value.writeToBuffer(),
      $0.GetConversationHistoryResponse.fromBuffer);
  static final _$channelIngest =
      $grpc.ClientMethod<$0.ChannelIngestRequest, $0.ChannelIngestAck>(
          '/tightbeam.v1.TightbeamController/ChannelIngest',
          ($0.ChannelIngestRequest value) => value.writeToBuffer(),
          $0.ChannelIngestAck.fromBuffer);
  static final _$channelReceive =
      $grpc.ClientMethod<$0.ChannelReceiveRequest, $0.ChannelOutbound>(
          '/tightbeam.v1.TightbeamController/ChannelReceive',
          ($0.ChannelReceiveRequest value) => value.writeToBuffer(),
          $0.ChannelOutbound.fromBuffer);
}

@$pb.GrpcServiceName('tightbeam.v1.TightbeamController')
abstract class TightbeamControllerServiceBase extends $grpc.Service {
  $core.String get $name => 'tightbeam.v1.TightbeamController';

  TightbeamControllerServiceBase() {
    $addMethod($grpc.ServiceMethod<$0.GetTurnRequest, $0.TurnAssignment>(
        'GetTurn',
        getTurn_Pre,
        false,
        false,
        ($core.List<$core.int> value) => $0.GetTurnRequest.fromBuffer(value),
        ($0.TurnAssignment value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.TurnResultChunk, $0.TurnAck>(
        'StreamTurnResult',
        streamTurnResult,
        true,
        false,
        ($core.List<$core.int> value) => $0.TurnResultChunk.fromBuffer(value),
        ($0.TurnAck value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.TurnRequest, $0.TurnEvent>(
        'Turn',
        turn_Pre,
        false,
        true,
        ($core.List<$core.int> value) => $0.TurnRequest.fromBuffer(value),
        ($0.TurnEvent value) => value.writeToBuffer()));
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
    $addMethod($grpc.ServiceMethod<$0.ChannelInbound, $0.ChannelOutbound>(
        'ChannelStream',
        channelStream,
        true,
        true,
        ($core.List<$core.int> value) => $0.ChannelInbound.fromBuffer(value),
        ($0.ChannelOutbound value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.SubscribeRequest, $0.UserMessage>(
        'Subscribe',
        subscribe_Pre,
        false,
        true,
        ($core.List<$core.int> value) => $0.SubscribeRequest.fromBuffer(value),
        ($0.UserMessage value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.RedeemEnrollmentRequest,
            $0.RedeemEnrollmentResponse>(
        'RedeemEnrollment',
        redeemEnrollment_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.RedeemEnrollmentRequest.fromBuffer(value),
        ($0.RedeemEnrollmentResponse value) => value.writeToBuffer()));
    $addMethod($grpc.ServiceMethod<$0.GetConversationHistoryRequest,
            $0.GetConversationHistoryResponse>(
        'GetConversationHistory',
        getConversationHistory_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.GetConversationHistoryRequest.fromBuffer(value),
        ($0.GetConversationHistoryResponse value) => value.writeToBuffer()));
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
  }

  $async.Future<$0.TurnAssignment> getTurn_Pre($grpc.ServiceCall $call,
      $async.Future<$0.GetTurnRequest> $request) async {
    return getTurn($call, await $request);
  }

  $async.Future<$0.TurnAssignment> getTurn(
      $grpc.ServiceCall call, $0.GetTurnRequest request);

  $async.Future<$0.TurnAck> streamTurnResult(
      $grpc.ServiceCall call, $async.Stream<$0.TurnResultChunk> request);

  $async.Stream<$0.TurnEvent> turn_Pre(
      $grpc.ServiceCall $call, $async.Future<$0.TurnRequest> $request) async* {
    yield* turn($call, await $request);
  }

  $async.Stream<$0.TurnEvent> turn(
      $grpc.ServiceCall call, $0.TurnRequest request);

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

  $async.Stream<$0.ChannelOutbound> channelStream(
      $grpc.ServiceCall call, $async.Stream<$0.ChannelInbound> request);

  $async.Stream<$0.UserMessage> subscribe_Pre($grpc.ServiceCall $call,
      $async.Future<$0.SubscribeRequest> $request) async* {
    yield* subscribe($call, await $request);
  }

  $async.Stream<$0.UserMessage> subscribe(
      $grpc.ServiceCall call, $0.SubscribeRequest request);

  $async.Future<$0.RedeemEnrollmentResponse> redeemEnrollment_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.RedeemEnrollmentRequest> $request) async {
    return redeemEnrollment($call, await $request);
  }

  $async.Future<$0.RedeemEnrollmentResponse> redeemEnrollment(
      $grpc.ServiceCall call, $0.RedeemEnrollmentRequest request);

  $async.Future<$0.GetConversationHistoryResponse> getConversationHistory_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.GetConversationHistoryRequest> $request) async {
    return getConversationHistory($call, await $request);
  }

  $async.Future<$0.GetConversationHistoryResponse> getConversationHistory(
      $grpc.ServiceCall call, $0.GetConversationHistoryRequest request);

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
}
