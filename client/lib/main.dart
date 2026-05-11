// Sycophant chat client. Phase 2 prototype: enrollment + bare chat.
//
// Two states:
//   1. Pre-enrollment: server URL + enrollment code paste → EnrollDevice
//      RPC → persist JWT → transition to chat
//   2. Post-enrollment: send message via Turn RPC, render streaming response
//
// JWT persistence uses shared_preferences. On PermissionDenied (typical when
// the 90-day JWT expires), the app surfaces a re-enroll prompt — Phase 2
// has no refresh; user pastes a fresh enrollment code from the operator.
import 'dart:async';

import 'package:flutter/material.dart';
// `grpc` exports a `ConnectionState` that collides with Flutter's
// AsyncSnapshot ConnectionState; we only use the ConnectionState from
// Flutter, so hide grpc's variant.
import 'package:grpc/grpc.dart' hide ConnectionState;
import 'package:shared_preferences/shared_preferences.dart';

import 'src/generated/tightbeam/v1/tightbeam.pbgrpc.dart';

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

/// Decides whether to show the enrollment screen or the chat screen based on
/// what's persisted. On first launch, prefs are empty → enrollment screen.
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

/// Persisted device credentials. Created post-enrollment.
class StoredCredentials {
  StoredCredentials({
    required this.serverHost,
    required this.serverPort,
    required this.jwt,
    required this.deviceId,
    required this.expiresAt,
  });

  static const _keyHost = 'server_host';
  static const _keyPort = 'server_port';
  static const _keyJwt = 'jwt';
  static const _keyDeviceId = 'device_id';
  static const _keyExpiresAt = 'expires_at';

  final String serverHost;
  final int serverPort;
  final String jwt;
  final String deviceId;
  final int expiresAt;

  static Future<StoredCredentials?> load() async {
    final prefs = await SharedPreferences.getInstance();
    final host = prefs.getString(_keyHost);
    final port = prefs.getInt(_keyPort);
    final jwt = prefs.getString(_keyJwt);
    final deviceId = prefs.getString(_keyDeviceId);
    final exp = prefs.getInt(_keyExpiresAt);
    if (host == null || port == null || jwt == null || deviceId == null || exp == null) {
      return null;
    }
    return StoredCredentials(
      serverHost: host,
      serverPort: port,
      jwt: jwt,
      deviceId: deviceId,
      expiresAt: exp,
    );
  }

  Future<void> persist() async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(_keyHost, serverHost);
    await prefs.setInt(_keyPort, serverPort);
    await prefs.setString(_keyJwt, jwt);
    await prefs.setString(_keyDeviceId, deviceId);
    await prefs.setInt(_keyExpiresAt, expiresAt);
  }

  static Future<void> clear() async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.remove(_keyHost);
    await prefs.remove(_keyPort);
    await prefs.remove(_keyJwt);
    await prefs.remove(_keyDeviceId);
    await prefs.remove(_keyExpiresAt);
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
      channel = ClientChannel(
        hostPort.$1,
        port: hostPort.$2,
        options: const ChannelOptions(credentials: ChannelCredentials.insecure()),
      );
      final client = TightbeamControllerClient(channel);
      final resp = await client.enrollDevice(
        EnrollRequest(enrollmentCode: code),
      );
      final creds = StoredCredentials(
        serverHost: hostPort.$1,
        serverPort: hostPort.$2,
        jwt: resp.jwt,
        deviceId: resp.deviceId,
        expiresAt: resp.expiresAt.toInt(),
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
              'Paste the enrollment code your operator generated for this '
              'device. Server is the tailnet hostname:port (default '
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
    final call = client.turn(
      req,
      options: CallOptions(
        metadata: {'authorization': 'Bearer ${widget.creds.jwt}'},
      ),
    );

    try {
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
              '[auth rejected - JWT expired or revoked. Sign out and re-enroll.]';
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
          'This deletes the device JWT from local storage. You will need a '
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
        title: Text(widget.creds.serverHost),
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
