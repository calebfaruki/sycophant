import 'dart:async';
import 'dart:typed_data';

import 'package:grpc/grpc.dart';

import 'generated/sycophant/common/v1/common.pb.dart';
import 'generated/tightbeam/v1/tightbeam.pbgrpc.dart';
import 'signed_request.dart';

/// Plain-Dart service shared by the chat surface, the skills row, and the
/// browser pane. Owns the `CallTool` invocation primitive and a
/// broadcast stream of `ServerRequest` frames the agent emits on the
/// channel. Lifetime is tied to `_ChatScreenState`.
class AgentSession {
  AgentSession({
    required this.channel,
    required this.workspace,
    required this.clientName,
    required this.keyPair,
  });

  final ClientChannel channel;
  final String workspace;
  final String clientName;
  final ClientKeyPair keyPair;

  final _serverRequestCtrl = StreamController<ServerRequest>.broadcast();
  Stream<ServerRequest> get serverRequests => _serverRequestCtrl.stream;

  /// Feed a ServerRequest frame from the receive stream into the session.
  /// `_onOutbound` calls this when it sees `ev.hasServerRequest()`.
  void handleServerRequest(ServerRequest req) {
    _serverRequestCtrl.add(req);
  }

  /// Invoke a workspace tool. Returns the raw `CallToolResponse`; callers
  /// decide how to render the `output` (treat `is_error` as the operative
  /// success/failure bit).
  Future<CallToolResponse> callTool(String name, String inputJson) async {
    final client = TightbeamGatewayClient(channel);
    final req = CallToolRequest()
      ..name = name
      ..inputJson = inputJson;
    final sig = buildSignedMetadata(
      method: TightbeamMethods.callTool,
      protobufBytes: Uint8List.fromList(req.writeToBuffer()),
      workspace: workspace,
      clientName: clientName,
      keyPair: keyPair,
    );
    return await client.callTool(
      req,
      options: CallOptions(metadata: sig.toMetadata()),
    );
  }

  /// Pre-mint a fresh conversation id. Used by the drawer's
  /// "+ New conversation" action so the new thread appears in the list
  /// immediately, before the user sends a message.
  Future<String> mintConversation() async {
    final client = TightbeamGatewayClient(channel);
    final req = MintConversationRequest();
    final sig = buildSignedMetadata(
      method: TightbeamMethods.mintConversation,
      protobufBytes: Uint8List.fromList(req.writeToBuffer()),
      workspace: workspace,
      clientName: clientName,
      keyPair: keyPair,
    );
    final resp = await client.mintConversation(
      req,
      options: CallOptions(metadata: sig.toMetadata()),
    );
    return resp.conversationId;
  }

  /// MRU-sorted list of conversations for this workspace. Reads the
  /// `conversations` field (new) — the deprecated `conversation_ids`
  /// flat list is ignored.
  Future<List<ConversationSummary>> listConversations() async {
    final client = TightbeamGatewayClient(channel);
    final req = ListConversationsRequest()..workspace = workspace;
    final sig = buildSignedMetadata(
      method: TightbeamMethods.listConversations,
      protobufBytes: Uint8List.fromList(req.writeToBuffer()),
      workspace: workspace,
      clientName: clientName,
      keyPair: keyPair,
    );
    final resp = await client.listConversations(
      req,
      options: CallOptions(metadata: sig.toMetadata()),
    );
    return resp.conversations;
  }

  /// Update a conversation's user-facing name. Persists to the
  /// controller's `meta.json` sidecar so the new name survives restart.
  /// Server caps name length and rejects over-long input with
  /// `InvalidArgument`; cross-workspace ids are rejected with
  /// `PermissionDenied`. Caller surfaces both as a snackbar + rollback.
  Future<void> setConversationName(
    String conversationId,
    String name,
  ) async {
    final client = TightbeamGatewayClient(channel);
    final req = SetConversationNameRequest()
      ..conversationId = conversationId
      ..name = name;
    final sig = buildSignedMetadata(
      method: TightbeamMethods.setConversationName,
      protobufBytes: Uint8List.fromList(req.writeToBuffer()),
      workspace: workspace,
      clientName: clientName,
      keyPair: keyPair,
    );
    await client.setConversationName(
      req,
      options: CallOptions(metadata: sig.toMetadata()),
    );
  }

  /// Permanently delete a conversation. Removes the in-memory entry
  /// and wipes the persisted log — no recovery. Caller should confirm
  /// with the user first.
  Future<void> deleteConversation(String conversationId) async {
    final client = TightbeamGatewayClient(channel);
    final req = DeleteConversationRequest()..conversationId = conversationId;
    final sig = buildSignedMetadata(
      method: TightbeamMethods.deleteConversation,
      protobufBytes: Uint8List.fromList(req.writeToBuffer()),
      workspace: workspace,
      clientName: clientName,
      keyPair: keyPair,
    );
    await client.deleteConversation(
      req,
      options: CallOptions(metadata: sig.toMetadata()),
    );
  }

  /// Fetch a conversation's persisted history for hydration on switch.
  /// `limit: 0` means server default; we keep it small here so first
  /// paint is fast.
  Future<List<HistoryEntry>> getConversationHistory(
    String conversationId,
  ) async {
    final client = TightbeamGatewayClient(channel);
    final req = GetConversationHistoryRequest()
      ..conversationId = conversationId;
    final sig = buildSignedMetadata(
      method: TightbeamMethods.getConversationHistory,
      protobufBytes: Uint8List.fromList(req.writeToBuffer()),
      workspace: workspace,
      clientName: clientName,
      keyPair: keyPair,
    );
    final resp = await client.getConversationHistory(
      req,
      options: CallOptions(metadata: sig.toMetadata()),
    );
    return resp.entries;
  }

  /// Poll the controller-owned turn phase for a conversation. Backs the
  /// client's reconciliation when a pushed `TurnStateEvent` was missed
  /// (reconnect, dropped receive stream). Returns the full event so the
  /// caller sees `state` plus `reason`/`code` on FAILED. An unknown /
  /// never-active conversation resolves to IDLE server-side, so this never
  /// throws NotFound for a fresh thread in the caller's own workspace.
  Future<TurnStateEvent> getTurnState(String conversationId) async {
    final client = TightbeamGatewayClient(channel);
    final req = GetTurnStateRequest()..conversationId = conversationId;
    final sig = buildSignedMetadata(
      method: TightbeamMethods.getTurnState,
      protobufBytes: Uint8List.fromList(req.writeToBuffer()),
      workspace: workspace,
      clientName: clientName,
      keyPair: keyPair,
    );
    return await client.getTurnState(
      req,
      options: CallOptions(metadata: sig.toMetadata()),
    );
  }

  void dispose() {
    _serverRequestCtrl.close();
  }
}
