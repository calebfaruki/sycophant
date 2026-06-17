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

  /// Permanently delete a conversation: removes it from the workspace's
  /// registry AND deletes every persisted event. No grace window; no
  /// recovery. The caller's workspace must own the conversation_id.
  $grpc.ResponseFuture<$0.DeleteConversationResponse> deleteConversation(
    $0.DeleteConversationRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$deleteConversation, request, options: options);
  }

  /// Update the user-facing name of a conversation. Persists to the
  /// controller's meta.json sidecar; survives restart. Server caps name
  /// length; the caller's workspace must own the conversation_id.
  $grpc.ResponseFuture<$0.SetConversationNameResponse> setConversationName(
    $0.SetConversationNameRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$setConversationName, request, options: options);
  }

  /// List the workspaces the calling client is authorized to act on.
  /// The only external RPC that carries no workspace claim: the call IS
  /// the authorization query. Verifier resolves the kid and returns the
  /// Client CR's spec.workspaces.
  $grpc.ResponseFuture<$0.ListWorkspacesResponse> listWorkspaces(
    $0.ListWorkspacesRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$listWorkspaces, request, options: options);
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

  /// External tool-catalog subscription. The controller forwards the
  /// calling workspace's transponder catalog as snapshots. Use it to
  /// render skill buttons, browser affordances, or any client-side UI
  /// driven by what the workspace exposes.
  $grpc.ResponseStream<$0.ToolListUpdate> watchTools(
    $0.WatchToolsRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(
        _$watchTools, $async.Stream.fromIterable([request]),
        options: options);
  }

  /// External tool invocation. The controller forwards to the calling
  /// workspace's transponder, which dispatches via its existing
  /// tool_router (no LLM involvement). Trust is structural: enrollment
  /// authenticates; transponder + chambers + gVisor + RBAC define what
  /// the tool actually does.
  $grpc.ResponseFuture<$0.CallToolResponse> callTool(
    $0.CallToolRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$callTool, request, options: options);
  }

  /// Internal: transponder dispatches a fire-and-forget client tool
  /// invocation. NOT in ALLOWED_METHODS — only the transponder pod's SA
  /// token (audience transponder.tightbeam) reaches this.
  $grpc.ResponseFuture<$0.SendServerNotificationResponse>
      sendServerNotification(
    $0.SendServerNotificationRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$sendServerNotification, request,
        options: options);
  }

  /// Internal: transponder dispatches a client tool and awaits a matching
  /// ClientResponse, blocking up to a server-side timeout. NOT in
  /// ALLOWED_METHODS.
  $grpc.ResponseFuture<$0.SendServerRequestAndAwaitResponse>
      sendServerRequestAndAwait(
    $0.SendServerRequestAndAwaitRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$sendServerRequestAndAwait, request,
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
  static final _$deleteConversation = $grpc.ClientMethod<
          $0.DeleteConversationRequest, $0.DeleteConversationResponse>(
      '/tightbeam.v1.TightbeamController/DeleteConversation',
      ($0.DeleteConversationRequest value) => value.writeToBuffer(),
      $0.DeleteConversationResponse.fromBuffer);
  static final _$setConversationName = $grpc.ClientMethod<
          $0.SetConversationNameRequest, $0.SetConversationNameResponse>(
      '/tightbeam.v1.TightbeamController/SetConversationName',
      ($0.SetConversationNameRequest value) => value.writeToBuffer(),
      $0.SetConversationNameResponse.fromBuffer);
  static final _$listWorkspaces =
      $grpc.ClientMethod<$0.ListWorkspacesRequest, $0.ListWorkspacesResponse>(
          '/tightbeam.v1.TightbeamController/ListWorkspaces',
          ($0.ListWorkspacesRequest value) => value.writeToBuffer(),
          $0.ListWorkspacesResponse.fromBuffer);
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
  static final _$watchTools =
      $grpc.ClientMethod<$0.WatchToolsRequest, $0.ToolListUpdate>(
          '/tightbeam.v1.TightbeamController/WatchTools',
          ($0.WatchToolsRequest value) => value.writeToBuffer(),
          $0.ToolListUpdate.fromBuffer);
  static final _$callTool =
      $grpc.ClientMethod<$0.CallToolRequest, $0.CallToolResponse>(
          '/tightbeam.v1.TightbeamController/CallTool',
          ($0.CallToolRequest value) => value.writeToBuffer(),
          $0.CallToolResponse.fromBuffer);
  static final _$sendServerNotification = $grpc.ClientMethod<
          $0.SendServerNotificationRequest, $0.SendServerNotificationResponse>(
      '/tightbeam.v1.TightbeamController/SendServerNotification',
      ($0.SendServerNotificationRequest value) => value.writeToBuffer(),
      $0.SendServerNotificationResponse.fromBuffer);
  static final _$sendServerRequestAndAwait = $grpc.ClientMethod<
          $0.SendServerRequestAndAwaitRequest,
          $0.SendServerRequestAndAwaitResponse>(
      '/tightbeam.v1.TightbeamController/SendServerRequestAndAwait',
      ($0.SendServerRequestAndAwaitRequest value) => value.writeToBuffer(),
      $0.SendServerRequestAndAwaitResponse.fromBuffer);
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
    $addMethod($grpc.ServiceMethod<$0.ListWorkspacesRequest,
            $0.ListWorkspacesResponse>(
        'ListWorkspaces',
        listWorkspaces_Pre,
        false,
        false,
        ($core.List<$core.int> value) =>
            $0.ListWorkspacesRequest.fromBuffer(value),
        ($0.ListWorkspacesResponse value) => value.writeToBuffer()));
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

  $async.Future<$0.ListWorkspacesResponse> listWorkspaces_Pre(
      $grpc.ServiceCall $call,
      $async.Future<$0.ListWorkspacesRequest> $request) async {
    return listWorkspaces($call, await $request);
  }

  $async.Future<$0.ListWorkspacesResponse> listWorkspaces(
      $grpc.ServiceCall call, $0.ListWorkspacesRequest request);

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
}

@$pb.GrpcServiceName('tightbeam.v1.TransponderControl')
class TransponderControlClient extends $grpc.Client {
  /// The hostname for this service.
  static const $core.String defaultHost = '';

  /// OAuth scopes needed for the client.
  static const $core.List<$core.String> oauthScopes = [
    '',
  ];

  TransponderControlClient(super.channel, {super.options, super.interceptors});

  /// Streams the unified tool catalog as snapshots. Push semantics — every
  /// catalog change emits a fresh snapshot, no diffing on the wire.
  $grpc.ResponseStream<$0.ToolListUpdate> watchTools(
    $0.WatchToolsRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createStreamingCall(
        _$watchTools, $async.Stream.fromIterable([request]),
        options: options);
  }

  /// Dispatches a tool call through the transponder's tool_router.
  /// No LLM involvement. The router routes by Source (Airlock, Mainframe,
  /// Runtime, or Channel — the last is reserved for client-implemented
  /// tools like RevealPath and would be a programmer error on this path).
  $grpc.ResponseFuture<$0.CallToolResponse> callTool(
    $0.CallToolRequest request, {
    $grpc.CallOptions? options,
  }) {
    return $createUnaryCall(_$callTool, request, options: options);
  }

  // method descriptors

  static final _$watchTools =
      $grpc.ClientMethod<$0.WatchToolsRequest, $0.ToolListUpdate>(
          '/tightbeam.v1.TransponderControl/WatchTools',
          ($0.WatchToolsRequest value) => value.writeToBuffer(),
          $0.ToolListUpdate.fromBuffer);
  static final _$callTool =
      $grpc.ClientMethod<$0.CallToolRequest, $0.CallToolResponse>(
          '/tightbeam.v1.TransponderControl/CallTool',
          ($0.CallToolRequest value) => value.writeToBuffer(),
          $0.CallToolResponse.fromBuffer);
}

@$pb.GrpcServiceName('tightbeam.v1.TransponderControl')
abstract class TransponderControlServiceBase extends $grpc.Service {
  $core.String get $name => 'tightbeam.v1.TransponderControl';

  TransponderControlServiceBase() {
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
