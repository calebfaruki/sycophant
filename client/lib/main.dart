// Sycophant chat client. ADR 013 client-signed flow:
//
//   1. Pre-enrollment: user pastes server + enrollment code; app
//      generates a P-256 keypair, calls RedeemEnrollment with the
//      public half, then calls ListWorkspaces with the freshly-redeemed
//      kid (no workspace claim — that RPC is the authorization query).
//      A picker resolves which workspace the user wants this device
//      bound to; keypair + chosen workspace + clientName persist to
//      secure storage.
//   2. Post-enrollment chat: the client acts as a channel adapter. It
//      opens a persistent ChannelReceive server-stream to receive
//      agent replies, and sends each user message via ChannelIngest
//      (unary). Both RPCs carry a per-request signed envelope
//      (x-sig-* metadata) verified by the controller's tower middleware.
//      Turn is internal-only — the workspace transponder is the sole
//      LLM-dispatch authority and applies AGENTS.md + the workspace's
//      tool catalog on every turn.
//
// On PermissionDenied (key rotated by operator, code reused, etc.) the
// app surfaces a re-enroll prompt — the user pastes a fresh code.

import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
// `grpc` exports a `ConnectionState` that collides with Flutter's
// AsyncSnapshot ConnectionState; we only use Flutter's variant.
import 'package:grpc/grpc.dart' hide ConnectionState;

import 'src/generated/tightbeam/v1/tightbeam.pbgrpc.dart';
import 'src/signed_request.dart';

void main() {
  runApp(const SycophantApp());
}

class SycophantApp extends StatelessWidget {
  const SycophantApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Sycophant',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.deepPurple),
        useMaterial3: true,
      ),
      home: const RootScreen(),
    );
  }
}

/// Decides whether to show the enrollment screen or the chat screen
/// based on what's in secure storage. First launch → enrollment.
class RootScreen extends StatefulWidget {
  const RootScreen({super.key});

  @override
  State<RootScreen> createState() => _RootScreenState();
}

class _RootScreenState extends State<RootScreen> {
  Future<StoredCredentials?>? _loaded;

  @override
  void initState() {
    super.initState();
    _loaded = StoredCredentials.load();
  }

  Future<void> _onEnrolled(StoredCredentials creds) async {
    await creds.persist();
    if (!mounted) return;
    setState(() {
      _loaded = Future.value(creds);
    });
  }

  Future<void> _onSignOut() async {
    await StoredCredentials.clear();
    if (!mounted) return;
    setState(() {
      _loaded = Future.value(null);
    });
  }

  @override
  Widget build(BuildContext context) {
    return FutureBuilder<StoredCredentials?>(
      future: _loaded,
      builder: (ctx, snap) {
        if (snap.connectionState != ConnectionState.done) {
          return const Scaffold(body: Center(child: CircularProgressIndicator()));
        }
        final creds = snap.data;
        if (creds == null) {
          return EnrollScreen(onEnrolled: _onEnrolled);
        }
        return ChatScreen(creds: creds, onSignOut: _onSignOut);
      },
    );
  }
}

/// Persisted device credentials. Holds the P-256 keypair (raw bytes,
/// Keychain/EncryptedSharedPreferences-backed via
/// `flutter_secure_storage`) plus the bits the operator chose at
/// enrollment time: server + workspace + clientName.
class StoredCredentials {
  StoredCredentials({
    required this.serverHost,
    required this.serverPort,
    required this.workspace,
    required this.clientName,
    required this.keyPair,
  });

  static const _keyHost = 'server_host';
  static const _keyPort = 'server_port';
  static const _keyWorkspace = 'workspace';
  static const _keyClientName = 'client_name';
  static const _keyPrivateScalar = 'priv_scalar_b64';
  static const _keyPublicSec1 = 'pub_sec1_b64';

  static const FlutterSecureStorage _storage = FlutterSecureStorage(
    mOptions: MacOsOptions(
      accessibility: KeychainAccessibility.first_unlock_this_device,
      synchronizable: false,
      accountName: 'md.sycophant.client',
    ),
  );

  final String serverHost;
  final int serverPort;
  final String workspace;
  final String clientName;
  final ClientKeyPair keyPair;

  static Future<StoredCredentials?> load() async {
    final host = await _storage.read(key: _keyHost);
    final portStr = await _storage.read(key: _keyPort);
    final workspace = await _storage.read(key: _keyWorkspace);
    final clientName = await _storage.read(key: _keyClientName);
    final scalarB64 = await _storage.read(key: _keyPrivateScalar);
    final sec1B64 = await _storage.read(key: _keyPublicSec1);
    if (host == null ||
        portStr == null ||
        workspace == null ||
        clientName == null ||
        scalarB64 == null ||
        sec1B64 == null) {
      return null;
    }
    final port = int.tryParse(portStr);
    if (port == null) return null;
    return StoredCredentials(
      serverHost: host,
      serverPort: port,
      workspace: workspace,
      clientName: clientName,
      keyPair: ClientKeyPair(
        privateScalar: base64.decode(scalarB64),
        publicSec1: base64.decode(sec1B64),
      ),
    );
  }

  Future<void> persist() async {
    await _storage.write(key: _keyHost, value: serverHost);
    await _storage.write(key: _keyPort, value: serverPort.toString());
    await _storage.write(key: _keyWorkspace, value: workspace);
    await _storage.write(key: _keyClientName, value: clientName);
    await _storage.write(
      key: _keyPrivateScalar,
      value: base64.encode(keyPair.privateScalar),
    );
    await _storage.write(
      key: _keyPublicSec1,
      value: base64.encode(keyPair.publicSec1),
    );
  }

  static Future<void> clear() async {
    await _storage.delete(key: _keyHost);
    await _storage.delete(key: _keyPort);
    await _storage.delete(key: _keyWorkspace);
    await _storage.delete(key: _keyClientName);
    await _storage.delete(key: _keyPrivateScalar);
    await _storage.delete(key: _keyPublicSec1);
  }
}

class EnrollScreen extends StatefulWidget {
  const EnrollScreen({super.key, required this.onEnrolled});

  final Future<void> Function(StoredCredentials) onEnrolled;

  @override
  State<EnrollScreen> createState() => _EnrollScreenState();
}

class _EnrollScreenState extends State<EnrollScreen> {
  final _serverCtrl = TextEditingController();
  final _codeCtrl = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _serverCtrl.dispose();
    _codeCtrl.dispose();
    super.dispose();
  }

  Future<void> _enroll() async {
    setState(() {
      _busy = true;
      _error = null;
    });

    final hostPort = _parseHostPort(_serverCtrl.text.trim());
    if (hostPort == null) {
      setState(() {
        _busy = false;
        _error = 'Server must be `host:port` (e.g. tightbeam:9090).';
      });
      return;
    }
    final code = _codeCtrl.text.trim();
    if (code.isEmpty) {
      setState(() {
        _busy = false;
        _error = 'Paste an enrollment code.';
      });
      return;
    }

    ClientChannel? channel;
    try {
      // Generate the keypair BEFORE the RPC; the public half travels in
      // the body, the private half stays on device for future signing.
      final keyPair = ClientKeyPair.generate();

      channel = ClientChannel(
        hostPort.$1,
        port: hostPort.$2,
        options: const ChannelOptions(
          credentials: ChannelCredentials.insecure(),
          connectionTimeout: Duration(seconds: 8),
        ),
      );
      final client = TightbeamControllerClient(channel);
      final resp = await client.redeemEnrollment(
        RedeemEnrollmentRequest(
          enrollmentCode: code,
          publicKey: keyPair.publicSec1,
        ),
      );

      // Now that the keypair is registered on the Client CR, ask the
      // server which workspaces this device is authorized for. The kid
      // is whatever name RedeemEnrollment echoed back.
      final workspaces = await _fetchAuthorizedWorkspaces(
        channel: channel,
        clientName: resp.clientName,
        keyPair: keyPair,
      );

      if (workspaces.isEmpty) {
        setState(() {
          _busy = false;
          _error =
              'Operator has not authorized any workspaces for this client. '
              'Ask them to add one to the Client CR\'s spec.workspaces, '
              'then re-enroll with a fresh code.';
        });
        return;
      }

      final String chosen;
      if (workspaces.length == 1) {
        chosen = workspaces.first;
      } else {
        if (!mounted) return;
        final pick = await _pickWorkspace(context, workspaces);
        if (pick == null) {
          setState(() {
            _busy = false;
            _error = 'Workspace selection cancelled.';
          });
          return;
        }
        chosen = pick;
      }

      final creds = StoredCredentials(
        serverHost: hostPort.$1,
        serverPort: hostPort.$2,
        workspace: chosen,
        clientName: resp.clientName,
        keyPair: keyPair,
      );
      await widget.onEnrolled(creds);
    } on GrpcError catch (e) {
      setState(() {
        _busy = false;
        _error = 'Enrollment rejected (${e.code}): ${e.message ?? 'no message'}';
      });
    } catch (e) {
      setState(() {
        _busy = false;
        _error = 'Connection failed: $e';
      });
    } finally {
      await channel?.shutdown();
    }
  }

  Future<String?> _pickWorkspace(
    BuildContext context,
    List<String> workspaces,
  ) {
    return showDialog<String>(
      context: context,
      builder: (ctx) => SimpleDialog(
        title: const Text('Choose a workspace'),
        children: workspaces
            .map(
              (w) => SimpleDialogOption(
                onPressed: () => Navigator.pop(ctx, w),
                child: Text(w),
              ),
            )
            .toList(),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Enroll device')),
      body: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            const Text(
              'Paste the enrollment code your operator generated. The '
              'app will fetch the workspaces your Client CR authorizes '
              'and prompt you to pick one. Server is the tailnet '
              'hostname:port (default "tightbeam:9090" matches the '
              'chart\'s tsnetBridge).',
            ),
            const SizedBox(height: 24),
            TextField(
              controller: _serverCtrl,
              decoration: const InputDecoration(
                labelText: 'Server (host:port)',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 16),
            TextField(
              controller: _codeCtrl,
              decoration: const InputDecoration(
                labelText: 'Enrollment code',
                hintText: 'eyJ0eXAiOiJKV1Qi...',
                border: OutlineInputBorder(),
              ),
              minLines: 3,
              maxLines: 5,
            ),
            const SizedBox(height: 16),
            if (_error != null)
              Padding(
                padding: const EdgeInsets.only(bottom: 12),
                child: Text(_error!, style: const TextStyle(color: Colors.red)),
              ),
            FilledButton(
              onPressed: _busy ? null : _enroll,
              child: Text(_busy ? 'Enrolling...' : 'Enroll'),
            ),
          ],
        ),
      ),
    );
  }
}

/// Call `ListWorkspaces` on the just-enrolled channel and return the
/// authorized workspace names. The signed envelope omits the workspace
/// header — `ListWorkspaces` is the only RPC that carries no workspace
/// claim, because the call itself is the authorization query.
Future<List<String>> _fetchAuthorizedWorkspaces({
  required ClientChannel channel,
  required String clientName,
  required ClientKeyPair keyPair,
}) async {
  final req = ListWorkspacesRequest();
  final sig = buildSignedMetadata(
    method: TightbeamMethods.listWorkspaces,
    protobufBytes: Uint8List.fromList(req.writeToBuffer()),
    clientName: clientName,
    keyPair: keyPair,
  );
  final client = TightbeamControllerClient(channel);
  final resp = await client.listWorkspaces(
    req,
    options: CallOptions(metadata: sig.toMetadata()),
  );
  return resp.workspaces;
}

class ChatScreen extends StatefulWidget {
  const ChatScreen({super.key, required this.creds, required this.onSignOut});

  final StoredCredentials creds;
  final Future<void> Function() onSignOut;

  @override
  State<ChatScreen> createState() => _ChatScreenState();
}

class _ChatScreenState extends State<ChatScreen> {
  final _inputCtrl = TextEditingController();
  final _scrollCtrl = ScrollController();
  final List<_Turn> _turns = [];
  bool _sending = false;
  ClientChannel? _channel;
  StreamSubscription<ChannelOutbound>? _outboundSub;
  _Turn? _pendingAssistant;

  /// Server-minted channel_id learned from the first ChannelAck frame on
  /// the ChannelReceive stream. Echoed verbatim on every ChannelIngest.
  /// Opaque to the client; only valid for the lifetime of the current
  /// ChannelReceive stream (a hot-restart opens a new stream → new id).
  String? _channelId;

  /// Conversation under which our messages are filed. Returned by the
  /// controller on each ChannelIngestAck; reserved for future
  /// GetConversationHistory replay across reconnects (not yet wired).
  // ignore: unused_field
  String? _conversationId;

  @override
  void initState() {
    super.initState();
    _channel = ClientChannel(
      widget.creds.serverHost,
      port: widget.creds.serverPort,
      options: const ChannelOptions(
        credentials: ChannelCredentials.insecure(),
        connectionTimeout: Duration(seconds: 8),
      ),
    );
    _openReceiveStream();
  }

  void _openReceiveStream() {
    final client = TightbeamControllerClient(_channel!);
    final req = ChannelReceiveRequest()
      ..adapterHint = 'flutter-app:${widget.creds.clientName}';
    final sig = buildSignedMetadata(
      method: TightbeamMethods.channelReceive,
      protobufBytes: Uint8List.fromList(req.writeToBuffer()),
      workspace: widget.creds.workspace,
      clientName: widget.creds.clientName,
      keyPair: widget.creds.keyPair,
    );
    final stream = client.channelReceive(
      req,
      options: CallOptions(metadata: sig.toMetadata()),
    );
    _outboundSub = stream.listen(
      _onOutbound,
      onError: _onReceiveError,
      onDone: _onReceiveDone,
    );
  }

  void _onOutbound(ChannelOutbound ev) {
    // First frame on the receive stream is a ChannelAck carrying the
    // server-minted channel_id we echo on subsequent ChannelIngest calls.
    if (ev.hasAck()) {
      setState(() {
        _channelId = ev.ack.channelId;
      });
      return;
    }
    if (!ev.hasSendMessage()) return;
    final send = ev.sendMessage;
    // ChannelSend carries the assistant's reply content as ContentBlocks.
    // Concatenate any text blocks into the pending assistant bubble.
    final text = send.content
        .where((b) => b.hasText())
        .map((b) => b.text.text)
        .join();
    if (text.isEmpty) return;
    setState(() {
      final assistant = _pendingAssistant;
      if (assistant != null) {
        assistant.text = assistant.text.isEmpty ? text : '${assistant.text}$text';
      } else {
        // Unsolicited outbound (e.g., a tool result pushed without a prior
        // user message). Append a fresh assistant turn.
        _turns.add(_Turn(role: _Role.assistant, text: text));
      }
      _sending = false;
      _pendingAssistant = null;
    });
    _scrollToBottom();
  }

  void _onReceiveError(Object e) {
    setState(() {
      _turns.add(_Turn(
        role: _Role.assistant,
        text: e is GrpcError && e.code == StatusCode.permissionDenied
            ? '[signature rejected — key may be rotated. Sign out and re-enroll.]'
            : '[receive stream error: $e]',
      ));
      _sending = false;
      _pendingAssistant = null;
    });
  }

  void _onReceiveDone() {
    // Server closed the stream (e.g., 55s drain timeout). Reopen so the
    // chat stays alive across long-idle sessions.
    if (mounted) {
      _openReceiveStream();
    }
  }

  @override
  void dispose() {
    _outboundSub?.cancel();
    _channel?.shutdown();
    _inputCtrl.dispose();
    _scrollCtrl.dispose();
    super.dispose();
  }

  Future<void> _send() async {
    final text = _inputCtrl.text.trim();
    if (text.isEmpty || _sending) return;
    _inputCtrl.clear();

    final userTurn = _Turn(role: _Role.user, text: text);
    final assistantTurn = _Turn(role: _Role.assistant, text: '');
    setState(() {
      _turns.add(userTurn);
      _turns.add(assistantTurn);
      _pendingAssistant = assistantTurn;
      _sending = true;
    });
    _scrollToBottom();

    final channelId = _channelId;
    if (channelId == null) {
      setState(() {
        assistantTurn.text =
            '[channel not yet registered — wait for the receive stream to open.]';
        _sending = false;
      });
      return;
    }

    final client = TightbeamControllerClient(_channel!);
    final req = ChannelIngestRequest()
      ..channelId = channelId
      ..userMessage = (UserMessage()
        ..sender = widget.creds.clientName
        ..content.add(
          ContentBlock()..text = (TextBlock()..text = text),
        ));

    try {
      final sig = buildSignedMetadata(
        method: TightbeamMethods.channelIngest,
        protobufBytes: Uint8List.fromList(req.writeToBuffer()),
        workspace: widget.creds.workspace,
        clientName: widget.creds.clientName,
        keyPair: widget.creds.keyPair,
      );
      final ack = await client.channelIngest(
        req,
        options: CallOptions(metadata: sig.toMetadata()),
      );
      // Capture conversation_id for future history-replay use.
      if (ack.conversationId.isNotEmpty) {
        _conversationId = ack.conversationId;
      }
      // Ack received; the agent's reply will arrive on the receive stream.
    } on GrpcError catch (e) {
      setState(() {
        if (e.code == StatusCode.permissionDenied) {
          assistantTurn.text =
              '[signature rejected — key may be rotated. Sign out and re-enroll.]';
        } else {
          assistantTurn.text = '[gRPC error ${e.code}: ${e.message ?? '?'}]';
        }
        _pendingAssistant = null;
        _sending = false;
      });
    } catch (e) {
      setState(() {
        assistantTurn.text = '[transport error: $e]';
        _pendingAssistant = null;
        _sending = false;
      });
    }
  }

  void _scrollToBottom() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!_scrollCtrl.hasClients) return;
      _scrollCtrl.animateTo(
        _scrollCtrl.position.maxScrollExtent,
        duration: const Duration(milliseconds: 150),
        curve: Curves.easeOut,
      );
    });
  }

  Future<void> _confirmSignOut() async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Sign out?'),
        content: const Text(
          'This deletes the keypair from local storage. You will need a '
          'fresh enrollment code from your operator to reconnect.',
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Sign out')),
        ],
      ),
    );
    if (ok == true) {
      await widget.onSignOut();
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text('${widget.creds.workspace} @ ${widget.creds.serverHost}'),
        actions: [
          IconButton(
            icon: const Icon(Icons.logout),
            tooltip: 'Sign out',
            onPressed: _confirmSignOut,
          ),
        ],
      ),
      body: Column(
        children: [
          Expanded(
            child: ListView.builder(
              controller: _scrollCtrl,
              padding: const EdgeInsets.all(12),
              itemCount: _turns.length,
              itemBuilder: (ctx, i) => _TurnBubble(turn: _turns[i]),
            ),
          ),
          SafeArea(
            top: false,
            child: Padding(
              padding: const EdgeInsets.all(8),
              child: Row(
                children: [
                  Expanded(
                    child: TextField(
                      controller: _inputCtrl,
                      decoration: const InputDecoration(
                        hintText: 'Message...',
                        border: OutlineInputBorder(),
                      ),
                      minLines: 1,
                      maxLines: 4,
                    ),
                  ),
                  const SizedBox(width: 8),
                  FilledButton(
                    onPressed: _sending ? null : _send,
                    child: const Icon(Icons.send),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

enum _Role { user, assistant }

class _Turn {
  _Turn({required this.role, required this.text});
  final _Role role;
  String text;
}

class _TurnBubble extends StatelessWidget {
  const _TurnBubble({required this.turn});
  final _Turn turn;

  @override
  Widget build(BuildContext context) {
    final isUser = turn.role == _Role.user;
    final bg = isUser
        ? Theme.of(context).colorScheme.primaryContainer
        : Theme.of(context).colorScheme.surfaceContainerHighest;
    return Align(
      alignment: isUser ? Alignment.centerRight : Alignment.centerLeft,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 4),
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: bg,
          borderRadius: BorderRadius.circular(12),
        ),
        constraints: const BoxConstraints(maxWidth: 320),
        child: SelectableText(
          turn.text.isEmpty ? '...' : turn.text,
        ),
      ),
    );
  }
}

(String, int)? _parseHostPort(String input) {
  final i = input.lastIndexOf(':');
  if (i <= 0 || i == input.length - 1) return null;
  final host = input.substring(0, i);
  final port = int.tryParse(input.substring(i + 1));
  if (port == null || port < 1 || port > 65535) return null;
  return (host, port);
}
