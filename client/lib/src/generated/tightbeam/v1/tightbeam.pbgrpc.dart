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

  /// List available models (used by transponder/router for model selection).
  $grpc.ResponseFuture<$0.ListModelsResponse> listModels(
    $0.ListModelsRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$listModels, request, options: options);
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

  /// Device enrollment. Phone (or other external client) presents a one-time
  /// enrollment code minted out-of-band by the operator (via
  /// `kubectl exec ... tightbeam-controller mint-enrollment <ws> <name>`).
  /// Controller validates the code's signature + expiry + claims, then mints
  /// a long-lived (90-day) device JWT and returns it. Client persists the
  /// JWT and presents it as `Authorization: Bearer <jwt>` on subsequent RPCs.
  /// EnrollDevice itself is unauthenticated — the enrollment code is the
  /// authentication artifact.
  $grpc.ResponseFuture<$0.EnrollResponse> enrollDevice(
    $0.EnrollRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$enrollDevice, request, options: options);
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
  static final _$listModels =
      $grpc.ClientMethod<$0.ListModelsRequest, $0.ListModelsResponse>(
          '/tightbeam.v1.TightbeamController/ListModels',
          ($0.ListModelsRequest value) => value.writeToBuffer(),
          $0.ListModelsResponse.fromBuffer);
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
  static final _$enrollDevice =
      $grpc.ClientMethod<$0.EnrollRequest, $0.EnrollResponse>(
          '/tightbeam.v1.TightbeamController/EnrollDevice',
          ($0.EnrollRequest value) => value.writeToBuffer(),
          $0.EnrollResponse.fromBuffer);
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
    $addMethod($grpc.ServiceMethod<$0.ListModelsRequest, $0.ListModelsResponse>(
        'ListModels',
        listModels_Pre,
        false,
        false,
        ($core.List<$core.int> value) => $0.ListModelsRequest.fromBuffer(value),
        ($0.ListModelsResponse value) => value.writeToBuffer()));
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
    $addMethod($grpc.ServiceMethod<$0.EnrollRequest, $0.EnrollResponse>(
        'EnrollDevice',
        enrollDevice_Pre,
        false,
        false,
        ($core.List<$core.int> value) => $0.EnrollRequest.fromBuffer(value),
        ($0.EnrollResponse value) => value.writeToBuffer()));
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

  $async.Future<$0.ListModelsResponse> listModels_Pre($grpc.ServiceCall $call,
      $async.Future<$0.ListModelsRequest> $request) async {
    return listModels($call, await $request);
  }

  $async.Future<$0.ListModelsResponse> listModels(
      $grpc.ServiceCall call, $0.ListModelsRequest request);

  $async.Stream<$0.ChannelOutbound> channelStream(
      $grpc.ServiceCall call, $async.Stream<$0.ChannelInbound> request);

  $async.Stream<$0.UserMessage> subscribe_Pre($grpc.ServiceCall $call,
      $async.Future<$0.SubscribeRequest> $request) async* {
    yield* subscribe($call, await $request);
  }

  $async.Stream<$0.UserMessage> subscribe(
      $grpc.ServiceCall call, $0.SubscribeRequest request);

  $async.Future<$0.EnrollResponse> enrollDevice_Pre(
      $grpc.ServiceCall $call, $async.Future<$0.EnrollRequest> $request) async {
    return enrollDevice($call, await $request);
  }

  $async.Future<$0.EnrollResponse> enrollDevice(
      $grpc.ServiceCall call, $0.EnrollRequest request);
}
