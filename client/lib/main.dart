// Sycophant chat client. ADR 013 client-signed flow:
//
//   1. Pre-enrollment: user pastes server + workspace + enrollment code;
//      app generates a P-256 keypair, calls RedeemEnrollment with the
//      public half, persists keypair + workspace + clientName to secure
//      storage.
//   2. Post-enrollment: every Turn RPC carries a per-request signed
//      envelope (x-sig-* metadata) verified by the controller's tower
//      middleware on the external listener.
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

  static const FlutterSecureStorage _storage = FlutterSecureStorage();

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
  final _serverCtrl = TextEditingController(text: 'tightbeam:9090');
  final _workspaceCtrl = TextEditingController();
  final _codeCtrl = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _serverCtrl.dispose();
    _workspaceCtrl.dispose();
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
    final workspace = _workspaceCtrl.text.trim();
    if (workspace.isEmpty) {
      setState(() {
        _busy = false;
        _error = 'Workspace required (must match a name in your Client CR).';
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
        options: const ChannelOptions(credentials: ChannelCredentials.insecure()),
      );
      final client = TightbeamControllerClient(channel);
      final resp = await client.redeemEnrollment(
        RedeemEnrollmentRequest(
          enrollmentCode: code,
          publicKey: keyPair.publicSec1,
        ),
      );
      final creds = StoredCredentials(
        serverHost: hostPort.$1,
        serverPort: hostPort.$2,
        workspace: workspace,
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
              'workspace must match a name in the Client CR\'s spec. '
              'Server is the tailnet hostname:port (default '
              '"tightbeam:9090" matches the chart\'s tsnetBridge).',
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
              controller: _workspaceCtrl,
              decoration: const InputDecoration(
                labelText: 'Workspace',
                hintText: 'hello-world',
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

  @override
  void initState() {
    super.initState();
    _channel = ClientChannel(
      widget.creds.serverHost,
      port: widget.creds.serverPort,
      options: const ChannelOptions(credentials: ChannelCredentials.insecure()),
    );
  }

  @override
  void dispose() {
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
      _sending = true;
    });
    _scrollToBottom();

    final client = TightbeamControllerClient(_channel!);
    final req = TurnRequest()
      ..messages.add(
        Message()
          ..role = 'user'
          ..content.add(
            ContentBlock()..text = (TextBlock()..text = text),
          ),
      );

    try {
      final sig = buildSignedMetadata(
        method: TightbeamMethods.turn,
        protobufBytes: Uint8List.fromList(req.writeToBuffer()),
        workspace: widget.creds.workspace,
        clientName: widget.creds.clientName,
        keyPair: widget.creds.keyPair,
      );
      final call = client.turn(
        req,
        options: CallOptions(metadata: sig.toMetadata()),
      );

      await for (final ev in call) {
        if (ev.hasContentDelta()) {
          setState(() {
            assistantTurn.text += ev.contentDelta.text;
          });
          _scrollToBottom();
        } else if (ev.hasComplete()) {
          break;
        } else if (ev.hasError()) {
          setState(() {
            assistantTurn.text = '[error: ${ev.error.message}]';
          });
          break;
        }
      }
    } on GrpcError catch (e) {
      if (e.code == StatusCode.permissionDenied) {
        setState(() {
          assistantTurn.text =
              '[signature rejected — key may be rotated. Sign out and re-enroll.]';
        });
      } else {
        setState(() {
          assistantTurn.text = '[gRPC error ${e.code}: ${e.message ?? '?'}]';
        });
      }
    } catch (e) {
      setState(() {
        assistantTurn.text = '[transport error: $e]';
      });
    } finally {
      setState(() {
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
