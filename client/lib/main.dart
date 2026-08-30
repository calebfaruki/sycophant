// Sycophant chat client. Client-signed flow:
//
//   1. Pre-enrollment: user pastes server + enrollment code; app
//      generates a P-256 keypair, calls RedeemCode with the
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
//      Turn is internal-only — the workspace harness is the sole
//      LLM-dispatch authority and applies AGENTS.md + the workspace's
//      tool catalog on every turn.
//
// On PermissionDenied (key rotated by operator, code reused, etc.) the
// app surfaces a re-enroll prompt — the user pastes a fresh code.

import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/foundation.dart' show kReleaseMode;
import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:gpt_markdown/gpt_markdown.dart';
// Typedefs + GptMarkdownConfig live in this sub-library; the main
// entrypoint imports it but does not re-export.
import 'package:gpt_markdown/custom_widgets/markdown_config.dart';
// `grpc` exports a `ConnectionState` that collides with Flutter's
// AsyncSnapshot ConnectionState; we only use Flutter's variant.
import 'package:grpc/grpc.dart' hide ConnectionState;

import 'src/agent_session.dart';
import 'src/browser_pane.dart';
import 'src/conversations_drawer.dart';
import 'src/generated/sycophant/common/v1/common.pb.dart';
import 'src/generated/relay/v1/relay.pbgrpc.dart';
import 'src/signed_request.dart';
import 'src/command_menu.dart';
import 'src/turn_parts.dart';

void main() {
  runApp(const SycophantApp());
}

/// Receive-stream reconnect backoff curve. Mirrors
/// `crates/shared/src/watcher_retry.rs:23-44`: start at `initial`,
/// double per failure, cap at `cap`, never give up. Reset to
/// `Duration.zero` on the first successful frame to start fresh on
/// the next outage.
///
/// Top-level so the unit test can import + assert the math without
/// pumping the widget tree.
@visibleForTesting
Duration nextReconnectDelay(Duration current, Duration initial, Duration cap) {
  if (current == Duration.zero) return initial;
  final doubled = Duration(milliseconds: current.inMilliseconds * 2);
  return doubled > cap ? cap : doubled;
}

/// Owns the receive-stream reconnect state: backoff curve, timer, and
/// the listen-wiring that demultiplexes data / error / done events into
/// UI side effects. Extracted from `_ChatScreenState` so the wiring is
/// testable without pumping the widget tree.
///
/// Callbacks are injected so the State remains the source of truth for
/// UI state (SnackBars, `setState`). The reconnector only tracks the
/// delay curve and the active subscription.
@visibleForTesting
class ReceiveReconnector {
  ReceiveReconnector({
    required this.initialDelay,
    required this.maxDelay,
    required this.onAck,
    required this.onFrame,
    required this.onFatalAuth,
    required this.onTransientError,
    required this.reopen,
    this.onDelayAdvance,
    this.idleTimeout,
  });

  final Duration initialDelay;
  final Duration maxDelay;
  final void Function(String channelId) onAck;
  final void Function(ChannelOutbound frame) onFrame;
  final void Function() onFatalAuth;
  final void Function() onTransientError;
  final void Function() reopen;
  final void Function(Duration delay)? onDelayAdvance;

  /// Max silence on a live stream before it's presumed half-open and force
  /// -reconnected. Reset by every inbound frame. Null disables the
  /// watchdog (the existing reconnect tests construct without it).
  final Duration? idleTimeout;

  Duration _delay = Duration.zero;
  Timer? _timer;
  Timer? _idleTimer;
  StreamSubscription<ChannelOutbound>? _sub;
  bool _disposed = false;

  /// Wire a fresh receive stream. Invariant: only `attach` reassigns
  /// `_sub`, so cancelling the subscription on error (cancelOnError:
  /// true) is safe — no other code path holds a reference.
  void attach(Stream<ChannelOutbound> stream) {
    _sub = stream.listen(
      _onData,
      onError: _onError,
      onDone: _onDone,
      cancelOnError: true,
    );
    _armIdle();
  }

  void _onData(ChannelOutbound ev) {
    // Any inbound frame (including the ack) proves the stream is live —
    // reset the idle watchdog.
    _armIdle();
    if (ev.hasAck()) {
      _delay = Duration.zero;
      onAck(ev.ack.channelId);
      return;
    }
    onFrame(ev);
  }

  /// (Re)start the idle watchdog. No-op when [idleTimeout] is unset.
  void _armIdle() {
    final t = idleTimeout;
    if (t == null) return;
    _idleTimer?.cancel();
    _idleTimer = Timer(t, _onIdle);
  }

  /// The stream went silent past [idleTimeout] with no error or done —
  /// presumed half-open (a dead socket the OS hasn't torn down). Drop it
  /// and reconnect immediately; if the server is genuinely down, the reopen
  /// errors and falls into the normal backoff path.
  void _onIdle() {
    if (_disposed) return;
    unawaited(_sub?.cancel());
    _sub = null;
    reopen();
  }

  void _onError(Object e) {
    _idleTimer?.cancel();
    final code = (e is GrpcError) ? e.code : null;
    if (isFatalAuthCode(code)) {
      onFatalAuth();
      return;
    }
    onTransientError();
    _scheduleReconnect();
  }

  /// Stream completed cleanly. With `cancelOnError: true`, this fires
  /// only on a server-clean close (no preceding error).
  void _onDone() {
    _idleTimer?.cancel();
    if (_disposed) return;
    _scheduleReconnect();
  }

  void _scheduleReconnect() {
    _timer?.cancel();
    _delay = nextReconnectDelay(_delay, initialDelay, maxDelay);
    onDelayAdvance?.call(_delay);
    _timer = Timer(_delay, reopen);
  }

  Future<void> dispose() async {
    _disposed = true;
    _timer?.cancel();
    _idleTimer?.cancel();
    await _sub?.cancel();
  }
}

class SycophantApp extends StatelessWidget {
  const SycophantApp({super.key});

  @override
  Widget build(BuildContext context) {
    final colorScheme = ColorScheme.fromSeed(seedColor: Colors.deepPurple);
    return MaterialApp(
      title: 'Sycophant',
      theme: ThemeData(
        colorScheme: colorScheme,
        useMaterial3: true,
        extensions: [
          GptMarkdownThemeData(
            brightness: Brightness.light,
            linkColor: colorScheme.primary,
            linkHoverColor: colorScheme.primary,
            hrLineColor: colorScheme.outlineVariant,
          ),
        ],
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

/// Shared secure-storage options. Unsigned local builds (debug/profile)
/// lack the application-identifier entitlement the data-protection keychain
/// requires, so they fall back to the legacy file keychain; signed release
/// builds keep the data-protection keychain.
const MacOsOptions _macOsKeychainOptions = MacOsOptions(
  accessibility: KeychainAccessibility.first_unlock_this_device,
  synchronizable: false,
  accountName: 'md.sycophant.client',
  useDataProtectionKeyChain: kReleaseMode,
);

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

  static const FlutterSecureStorage _storage =
      FlutterSecureStorage(mOptions: _macOsKeychainOptions);

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
    await StoredConversations._storage.delete(key: StoredConversations._key);
  }
}

/// Per-workspace active conversation cursor. Single JSON blob keyed
/// `conv_id_by_workspace_v1` so a fresh enrollment for a different
/// workspace doesn't strand orphan secure-storage keys.
class StoredConversations {
  static const _key = 'conv_id_by_workspace_v1';
  static const FlutterSecureStorage _storage =
      FlutterSecureStorage(mOptions: _macOsKeychainOptions);

  static Future<String?> read(String workspace) async {
    final raw = await _storage.read(key: _key);
    if (raw == null) return null;
    try {
      final m = jsonDecode(raw) as Map<String, dynamic>;
      final v = m[workspace];
      return v is String && v.isNotEmpty ? v : null;
    } catch (_) {
      return null;
    }
  }

  static Future<void> write(String workspace, String convId) async {
    final raw = await _storage.read(key: _key);
    final m = raw == null
        ? <String, dynamic>{}
        : (jsonDecode(raw) as Map<String, dynamic>);
    m[workspace] = convId;
    await _storage.write(key: _key, value: jsonEncode(m));
  }

  static Future<void> clearWorkspace(String workspace) async {
    final raw = await _storage.read(key: _key);
    if (raw == null) return;
    try {
      final m = jsonDecode(raw) as Map<String, dynamic>;
      m.remove(workspace);
      await _storage.write(key: _key, value: jsonEncode(m));
    } catch (_) {
      // Corrupt blob — just clear the key.
      await _storage.delete(key: _key);
    }
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
        _error = 'Server must be `host:port` (e.g. relay:9090).';
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
      final client = RelayGatewayClient(channel);
      final resp = await client.redeemCode(
        RedeemCodeRequest(
          code: code,
          publicKey: keyPair.publicSec1,
        ),
      );

      // Now that the keypair is registered against the grant row, ask the
      // server which workspaces this device is authorized for. The kid
      // is whatever name RedeemCode echoed back.
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
              'hostname:port (default "relay:9090" matches the '
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
    method: RelayMethods.listWorkspaces,
    protobufBytes: Uint8List.fromList(req.writeToBuffer()),
    clientName: clientName,
    keyPair: keyPair,
  );
  final client = RelayGatewayClient(channel);
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

/// Single source of truth for the chat indicator state.
///
/// - `idle`: no active turn. Composer enabled.
/// - `sending`: between the user pressing send and the controller's
///   `ChannelIngest` ack OR the first cluster-pushed `WORKING` event.
///   Pure client-derived; only the client knows the user just hit submit.
/// - `working`: cluster reports a turn is in flight. Driven by the
///   `TurnStateEvent` frames on `ChannelReceive`.
/// - `failed`: cluster reported the turn ended in failure (prompt job
///   reaped/crashed, idle-timeout, persist failure). Driven by a `FAILED`
///   `TurnStateEvent`. Carries a reason for display and re-enables the
///   composer so a resend retries the turn.
enum TurnPhase { idle, sending, working, failed }

/// Device-renderable tools this client advertises in `supported_methods` on
/// every `ChannelIngest`. The gateway rejects any un-advertised method
/// server-side before it reaches the client (capability negotiation, not
/// client-side refusal), so a method dropped here can never be rendered.
/// `RevealPath` is the existing fire-and-forget template; the two HITL tools
/// are the inbound round-trip pair.
const List<String> deviceRenderableMethods = [
  'RevealPath',
  'RequestUserInput',
  'RequestUserAuth',
];

/// Result payload the client returns as a `RequestUserInput` tool call's
/// result: the chosen `action_id` plus optional arguments. Correlated back to
/// the awaiting server request by `request_id` on the enclosing
/// `ClientResponse`.
@visibleForTesting
String hitlInputResult(String actionId, Map<String, dynamic>? arguments) {
  final result = <String, dynamic>{'action_id': actionId};
  if (arguments != null) result['arguments'] = arguments;
  return jsonEncode(result);
}

/// Result payload the client returns when a `RequestUserAuth` external
/// callback resolves. A resolved auth reports success, not an error.
@visibleForTesting
String hitlAuthResult() => jsonEncode({'ok': true});

/// Build a `CancelTurn` request for a turn's identifier (the conversation
/// id — one turn is in flight per conversation). Pure + top-level so the
/// keying is unit-testable without a live channel.
@visibleForTesting
CancelTurnRequest buildCancelTurnRequest(String conversationId) =>
    CancelTurnRequest()..conversationId = conversationId;

/// A collapsible group of streamed sub-agent items, nested under the parent
/// turn. Collapsed by default (child content hidden until the header is
/// tapped) via the native `ExpansionTile`.
class SubagentGroupTile extends StatelessWidget {
  const SubagentGroupTile({
    super.key,
    required this.childConversationId,
    this.name,
    required this.children,
  });

  final String childConversationId;

  /// Operator-authored sub-agent name. When non-empty, labels the tile; else
  /// the header falls back to the child-conversation id prefix.
  final String? name;
  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final label = (name != null && name!.isNotEmpty)
        ? name!
        : 'Sub-agent '
            '${childConversationId.substring(0, childConversationId.length.clamp(0, 8))}';
    return ExpansionTile(
      title: Text(label),
      childrenPadding: const EdgeInsets.only(left: 16),
      children: children,
    );
  }
}

/// Per-conversation tracker of the last-seen `system_prompt_sha256`. Surfaces
/// a prompt-change warning when a turn's hash differs from the prior turn's in
/// the same conversation. Per-conversation (not global) so switching
/// conversations never spuriously warns, and the first turn (no prior hash)
/// never warns. Testable without pumping the tree.
@visibleForTesting
class PromptChangeTracker {
  final Map<String, String> _lastByConv = {};

  /// Record `hash` for `convId` and return true iff it differs from the prior
  /// hash stored for that same conversation. First observation → false.
  bool observe(String convId, String hash) {
    final prior = _lastByConv[convId];
    _lastByConv[convId] = hash;
    return prior != null && prior != hash;
  }
}

/// Map a cluster `TurnState` to the UI `TurnPhase`, or `null` for states
/// the indicator does not render (UNSPECIFIED, and the reserved
/// THINKING/STOPPING slots). Pure + top-level so the mapping is unit
/// testable without pumping the widget tree.
@visibleForTesting
TurnPhase? turnPhaseFromState(TurnState state) {
  if (state == TurnState.TURN_STATE_WORKING) return TurnPhase.working;
  if (state == TurnState.TURN_STATE_IDLE) return TurnPhase.idle;
  if (state == TurnState.TURN_STATE_FAILED) return TurnPhase.failed;
  // A client-cancelled turn is terminal but NOT an error: re-enable input
  // with no error banner, exactly like idle.
  if (state == TurnState.TURN_STATE_CANCELLED) return TurnPhase.idle;
  return null;
}

/// The gRPC status codes that mean the client is dead until re-enrollment:
/// a rejected or rotated signature (`permissionDenied`/`unauthenticated`), or
/// a version-skewed gateway that no longer implements the RPC after an upgrade
/// (`unimplemented`). Shared by the receive stream and the send path so the two
/// classifications never drift.
bool isFatalAuthCode(int? code) =>
    code == StatusCode.permissionDenied ||
    code == StatusCode.unauthenticated ||
    code == StatusCode.unimplemented;

/// How a failed `ChannelIngest` on the send path should surface. A fatal-auth
/// code (see [isFatalAuthCode]) — a rejected/rotated signature, or a
/// version-skewed gateway that no longer implements the RPC — means the client
/// is dead until re-enrollment, so it routes to the persistent sign-out prompt,
/// never an inline assistant bubble. Everything else is a transport hiccup
/// shown inline. Pure + top-level so the classification is unit testable
/// without pumping the widget tree.
enum SendFailure { fatalAuth, transport }

@visibleForTesting
SendFailure sendFailureDisposition(Object error) =>
    (error is GrpcError && isFatalAuthCode(error.code))
        ? SendFailure.fatalAuth
        : SendFailure.transport;

/// Read a skill's markdown body through the harness-local `Skill` tool, the
/// same path the LLM uses. Trimmed, so the caller's empty check catches a
/// whitespace-only file. Throws when the tool reports an error. Pure +
/// top-level so the fetch is unit testable without pumping the widget tree.
@visibleForTesting
Future<String> fetchSkillBody(
  AgentSession session,
  String name, {
  required String conversationId,
}) async {
  final body = await callToolText(
    session,
    'Skill',
    jsonEncode({'name': name}),
    conversationId: conversationId,
  );
  return body.trim();
}

/// Single-writer owner of per-conversation turn phase + failure reason.
/// Extracted from `_ChatScreenState` so the transition rules are testable
/// without pumping the widget tree.
///
/// Two write paths with deliberately different authority:
/// - [applyPush]: an authoritative `ChannelReceive` `TurnStateEvent`, or a
///   client-local transition (the user hitting send). Always applies.
/// - [applyPoll]: a `GetTurnState` poll result. Reconciles a *missed*
///   terminal: it acts ONLY when the conversation is currently `working`,
///   and then only to a terminal (`idle`/`failed`). This makes the poll
///   downgrade-only — a poll never upgrades `idle`→`working` (stale
///   last-turn-state lag) and a late response can't clobber a fresh send.
@visibleForTesting
class TurnStateReconciler {
  final Map<String, TurnPhase> _phaseByConv = {};
  final Map<String, String> _reasonByConv = {};

  /// Sentinel key for the "no conversation minted yet" phase, used between
  /// the user pressing send and the controller stamping a real id.
  static const preMintKey = '';

  String _key(String? convId) => convId ?? preMintKey;

  TurnPhase phaseFor(String? convId) =>
      _phaseByConv[_key(convId)] ?? TurnPhase.idle;

  /// The failure reason, surfaced only while the conversation is `failed`.
  String? reasonFor(String? convId) {
    final key = _key(convId);
    return _phaseByConv[key] == TurnPhase.failed ? _reasonByConv[key] : null;
  }

  /// Authoritative transition. Always wins.
  void applyPush(String? convId, TurnPhase phase, {String reason = ''}) {
    _set(_key(convId), phase, reason);
  }

  /// Poll-driven reconciliation. No-op unless the conversation is currently
  /// `working`; from there it may settle to `idle`/`failed` but never
  /// re-assert `working`.
  void applyPoll(String? convId, TurnPhase phase, {String reason = ''}) {
    final key = _key(convId);
    if (_phaseByConv[key] != TurnPhase.working) return;
    if (phase == TurnPhase.working) return;
    _set(key, phase, reason);
  }

  /// Carry an in-flight entry from the pre-mint key to a freshly stamped id.
  void carry(String from, String to) {
    final p = _phaseByConv.remove(from);
    final r = _reasonByConv.remove(from);
    if (p != null) _phaseByConv[to] = p;
    if (r != null) _reasonByConv[to] = r;
  }

  void forget(String convId) {
    _phaseByConv.remove(convId);
    _reasonByConv.remove(convId);
  }

  void clearAll() {
    _phaseByConv.clear();
    _reasonByConv.clear();
  }

  void _set(String key, TurnPhase phase, String reason) {
    _phaseByConv[key] = phase;
    if (phase == TurnPhase.failed) {
      _reasonByConv[key] = reason.isNotEmpty ? reason : 'The turn failed.';
    } else {
      _reasonByConv.remove(key);
    }
  }
}

/// Periodic, gated poller for turn state. Fires [poll] every [interval]
/// only while [shouldPoll] returns true, so it costs nothing when no turn
/// is in flight. Extracted (like `ReceiveReconnector`) so the gating is
/// testable with fakeAsync, without the widget tree or a live channel.
@visibleForTesting
class TurnStatePoller {
  TurnStatePoller({
    required this.interval,
    required this.shouldPoll,
    required this.poll,
  });

  final Duration interval;
  final bool Function() shouldPoll;
  final Future<void> Function() poll;

  Timer? _timer;

  void start() {
    _timer?.cancel();
    _timer = Timer.periodic(interval, (_) => tick());
  }

  /// One poll cycle: invoke [poll] only when [shouldPoll] is true. Exposed
  /// so the gate is verifiable without elapsing a real timer (the periodic
  /// firing itself is `Timer.periodic`).
  @visibleForTesting
  void tick() {
    if (shouldPoll()) unawaited(poll());
  }

  void dispose() {
    _timer?.cancel();
  }
}

/// Backstop for a turn stuck `working` long past any normal duration —
/// when both the cluster push and the periodic poll failed to surface a
/// terminal. On expiry it runs a poll-then-reassure cycle: it NEVER
/// unilaterally fails a turn (only the cluster declares failure), it just
/// re-confirms state and, if still working, reassures the user and waits
/// again. Extracted (like the poller) so its arm/disarm semantics are
/// testable with virtual time.
@visibleForTesting
class DeadmanWatchdog {
  DeadmanWatchdog({required this.timeout, required this.onExpired});

  final Duration timeout;
  final void Function() onExpired;

  Timer? _timer;

  bool get isArmed => _timer != null;

  /// Start the countdown. Idempotent while already armed, so repeated
  /// working-confirmations (e.g. a 7s poll re-reading `working`) don't keep
  /// pushing the deadline out — the deadman measures time since the turn
  /// began working, not time since the last confirmation.
  void arm() {
    if (_timer != null) return;
    _timer = Timer(timeout, () {
      _timer = null;
      onExpired();
    });
  }

  void disarm() {
    _timer?.cancel();
    _timer = null;
  }

  void dispose() => disarm();
}

class _ChatScreenState extends State<ChatScreen> {
  final _inputCtrl = TextEditingController();
  final _scrollCtrl = ScrollController();
  final List<_Turn> _turns = [];
  ClientChannel? _channel;
  ReceiveReconnector? _reconnector;
  AgentSession? _session;
  final _browserKey = GlobalKey<BrowserPaneState>();
  final _scaffoldKey = GlobalKey<ScaffoldState>();

  /// Server-minted channel_id learned from the first ChannelAck frame on
  /// the ChannelReceive stream. Echoed verbatim on every ChannelIngest.
  /// Opaque to the client; only valid for the lifetime of the current
  /// ChannelReceive stream (a hot-restart opens a new stream → new id).
  String? _channelId;

  /// The active conversation. `null` means "next _send should ingest
  /// with an empty conversation_id so the controller mints a fresh
  /// one." Persisted per-workspace via `StoredConversations`.
  String? _activeConvId;

  /// The workspace's grant menu, fetched once per session.
  /// Each entry pairs a toolset with the grant names it may use.
  List<ToolsetGrants> _grantMenu = [];

  /// Grants the user has toggled on, keyed by toolset. Attached to every
  /// outgoing message; the harness injects them into that turn's tool calls.
  final Map<String, String> _selectedGrants = {};

  /// The assistant turn currently receiving streamed item frames, if any.
  /// Cleared when the turn finalizes (terminal turn_state) or the
  /// conversation changes. Item frames append their parts here.
  _Turn? _streamingTurn;

  /// Per-conversation turn-phase state. Switching to a conversation
  /// shows whatever phase that conversation was last known to be in
  /// (defaults to idle for never-seen ids). Allows mid-turn switches
  /// without indicator confusion.
  /// Single-writer owner of per-conversation turn phase + failure reason.
  /// All indicator state flows through this; the State only reads it.
  final TurnStateReconciler _reconciler = TurnStateReconciler();

  /// Bumped whenever a fresh conversation id is stamped onto an ack —
  /// signals the drawer to refetch `ListConversations`.
  int _drawerRefreshTick = 0;

  /// Per-conversation last-seen `system_prompt_sha256`. Raises a
  /// prompt-change warning when a turn-start hash differs from the prior
  /// turn's in the same conversation.
  final PromptChangeTracker _promptTracker = PromptChangeTracker();

  /// Agent identity for the active conversation, learned from the
  /// harness-emitted turn-start `TurnStateEvent`. Rendered in the header.
  String? _activeAgentName;

  /// True when the active conversation's latest turn-start carried a
  /// `system_prompt_sha256` different from the prior turn's. Surfaced as a
  /// banner until the next send.
  bool _promptChanged = false;

  /// Sub-agent items streamed under the active turn, grouped by the child
  /// conversation id. Rebuilt per active conversation; frames whose
  /// `parent_conversation_id` matches the active conversation route here
  /// instead of flattening into the top-level turn.
  SubagentGroups? _subagentGroups;

  TurnPhase get _activePhase => _reconciler.phaseFor(_activeConvId);

  /// Failure reason for the active conversation, or null when it is not in
  /// the failed phase. Drives the indicator's error affordance.
  String? get _activeFailureReason => _reconciler.reasonFor(_activeConvId);
  /// Last received error message — used to dedupe SnackBar spam.
  String? _lastReceiveError;

  /// Receive-stream backoff bounds. Mirrors the Rust-side
  /// `shared::watcher_retry::run_watcher_forever` curve: 1s → ×2 → cap
  /// 30s. The reconnector owns the active timer + delay state.
  static const Duration _reconnectInitial = Duration(seconds: 1);
  static const Duration _reconnectMax = Duration(seconds: 30);

  /// Turn-state poll cadence. Slow on purpose: it's a reconciliation
  /// fallback for missed pushes (dropped receive stream, reconnect), not
  /// the primary signal — the `ChannelReceive` push remains authoritative.
  static const Duration _pollInterval = Duration(seconds: 7);
  TurnStatePoller? _poller;

  /// Deadman backstop: a turn stuck `working` this long, with neither push
  /// nor poll surfacing a terminal, triggers a poll-then-reassure cycle
  /// (never a unilateral failure).
  static const Duration _deadmanTimeout = Duration(seconds: 330);
  DeadmanWatchdog? _deadman;

  /// Force-reconnect the receive stream after this much silence — catches a
  /// half-open socket that delivers neither frames nor an error.
  static const Duration _receiveIdleTimeout = Duration(seconds: 100);

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
    _session = AgentSession(
      channel: _channel!,
      workspace: widget.creds.workspace,
      clientName: widget.creds.clientName,
      keyPair: widget.creds.keyPair,
    );
    _reconnector = ReceiveReconnector(
      initialDelay: _reconnectInitial,
      maxDelay: _reconnectMax,
      onAck: _onChannelAck,
      onFrame: _onOutboundFrame,
      onFatalAuth: () => _onReceiveStatus(fatal: true),
      onTransientError: () => _onReceiveStatus(fatal: false),
      reopen: _openReceiveStream,
      idleTimeout: _receiveIdleTimeout,
    );
    _openReceiveStream();
    // Reconciliation poll: while a turn is `working`, periodically confirm
    // the cluster-owned phase so a missed terminal push (dropped stream)
    // still settles the indicator. Gated to `working` so it's free at rest.
    _poller = TurnStatePoller(
      interval: _pollInterval,
      shouldPoll: () => _activePhase == TurnPhase.working,
      poll: _pollTurnStateOnce,
    )..start();
    _deadman = DeadmanWatchdog(
      timeout: _deadmanTimeout,
      onExpired: _onDeadmanExpired,
    );
    // Restore the most recently active conversation for this workspace
    // and hydrate the chat from its persisted history. Fire-and-forget;
    // failures here just leave the UI in the "no active conversation"
    // state — the user can pick from the drawer.
    unawaited(_restoreActiveConversation());
    // Fetch the workspace's grant menu. Fire-and-forget; a
    // failure leaves the chips row absent and every send grantless.
    unawaited(_fetchGrantMenu());
  }

  Future<void> _fetchGrantMenu() async {
    try {
      final req = ListGrantsRequest()..workspace = widget.creds.workspace;
      final sig = buildSignedMetadata(
        method: RelayMethods.listGrants,
        protobufBytes: Uint8List.fromList(req.writeToBuffer()),
        workspace: widget.creds.workspace,
        clientName: widget.creds.clientName,
        keyPair: widget.creds.keyPair,
      );
      final resp = await RelayGatewayClient(_channel!).listGrants(
        req,
        options: CallOptions(metadata: sig.toMetadata()),
      );
      if (!mounted) return;
      setState(() => _grantMenu = resp.toolsets);
    } catch (e) {
      debugPrint('list grants failed: $e');
    }
  }

  Future<void> _restoreActiveConversation() async {
    final convId =
        await StoredConversations.read(widget.creds.workspace);
    if (!mounted || convId == null) return;
    setState(() {
      _activeConvId = convId;
    });
    await _hydrateHistory(convId);
  }

  Future<void> _hydrateHistory(String convId) async {
    final session = _session;
    if (session == null) return;
    try {
      final entries = await session.getConversationHistory(convId);
      if (!mounted || _activeConvId != convId) return;
      // Race guard: a freshly-minted conversation, or one with an
      // in-flight user message the server hasn't appended yet, returns
      // empty entries. Clobbering `_turns` here would drop the local
      // user bubble added by `_send`. Only replace when the server has
      // something to show.
      if (entries.isEmpty) return;
      setState(() {
        _turns.clear();
        _streamingTurn = null;
        _turns.addAll(_turnsFromHistory(entries));
      });
      _scrollToBottom();
    } catch (e) {
      _showErrorSnack('Could not load conversation history: $e');
    }
  }


  void _openReceiveStream() {
    if (!mounted) return;
    final client = RelayGatewayClient(_channel!);
    final req = ChannelReceiveRequest()
      ..adapterHint = 'flutter-app:${widget.creds.clientName}';
    final sig = buildSignedMetadata(
      method: RelayMethods.channelReceive,
      protobufBytes: Uint8List.fromList(req.writeToBuffer()),
      workspace: widget.creds.workspace,
      clientName: widget.creds.clientName,
      keyPair: widget.creds.keyPair,
    );
    final stream = client.channelReceive(
      req,
      options: CallOptions(metadata: sig.toMetadata()),
    );
    _reconnector?.attach(stream);
  }

  void _onChannelAck(String channelId) {
    if (!mounted) return;
    setState(() {
      _channelId = channelId;
      _lastReceiveError = null;
    });
    // A fresh receive stream just opened. If a turn was in flight across
    // the gap, its terminal push may have been missed — reconcile once
    // against the controller's recorded phase (downgrade-only, so a still
    // -running turn is left untouched).
    unawaited(_pollTurnStateOnce());
  }

  /// Poll the cluster turn-state for the active conversation and reconcile
  /// (downgrade-only via `applyPoll`). No-op unless a turn is `working`.
  /// Best-effort: a transient failure just waits for the next poll. Shared
  /// by the periodic poller and the reconnect reconcile.
  Future<void> _pollTurnStateOnce() async {
    if (!mounted || _activePhase != TurnPhase.working) return;
    final convId = _activeConvId;
    final session = _session;
    if (convId == null || session == null) return;
    try {
      final ev = await session.getTurnState(convId);
      if (!mounted) return;
      final phase = turnPhaseFromState(ev.state);
      if (phase == null) return;
      setState(() {
        _reconciler.applyPoll(convId, phase, reason: ev.reason);
      });
      _refreshDeadman();
    } catch (_) {
      // Reconciliation is best-effort; the next poll retries.
    }
  }

  /// Arm the deadman while the active conversation is `working`; disarm
  /// otherwise. Idempotent (arm-while-armed is a no-op), so it's safe to
  /// call after any phase change.
  void _refreshDeadman() {
    if (_activePhase == TurnPhase.working) {
      _deadman?.arm();
    } else {
      _deadman?.disarm();
    }
  }

  /// Deadman fired: the active turn has been `working` far longer than
  /// normal with no terminal from push or poll. Poll-then-reassure —
  /// re-confirm cluster state, and if it's still working, reassure the user
  /// and wait another cycle. NEVER fail unilaterally: only the cluster
  /// declares a turn failed.
  void _onDeadmanExpired() {
    if (!mounted) return;
    unawaited(_pollTurnStateOnce().then((_) {
      if (!mounted || _activePhase != TurnPhase.working) return;
      _showErrorSnack('Still working — this turn is taking longer than usual…');
      _deadman?.arm();
    }));
  }

  void _onOutboundFrame(ChannelOutbound ev) {
    // Cluster-pushed turn-phase events. WORKING fires when the controller
    // has routed the user message to a workspace harness; IDLE fires
    // after the assistant SendMessage is enqueued on this same mpsc —
    // FIFO ordering guarantees the bubble lands before the indicator
    // collapses. FAILED fires when the turn was torn down (prompt job
    // reaped/crashed, idle-timeout, persist failure) and carries a reason.
    // THINKING/STOPPING are reserved on the wire and ignored here.
    if (ev.hasTurnState()) {
      final ts = ev.turnState;
      // Empty conversation_id = channel-wide replay frame. Skip — we
      // only track per-conversation indicator state, and the channel
      // doesn't have a single conversation.
      if (ts.conversationId.isEmpty) return;
      final phase = turnPhaseFromState(ts.state);
      if (phase == null) return;
      // Set when this terminal frame ends a tool-bearing streamed turn:
      // its result only lands in history, so we refetch to render it.
      var refetchForTool = false;
      setState(() {
        _reconciler.applyPush(ts.conversationId, phase, reason: ts.reason);
        // Turn-start identity + prompt-change (the harness stamps
        // agent_name / system_prompt_sha256 on the WORKING turn-start frame).
        if (phase == TurnPhase.working) {
          if (ts.systemPromptSha256.isNotEmpty) {
            final changed = _promptTracker.observe(
              ts.conversationId,
              ts.systemPromptSha256,
            );
            if (ts.conversationId == _activeConvId) _promptChanged = changed;
          }
          if (ts.conversationId == _activeConvId && ts.agentName.isNotEmpty) {
            _activeAgentName = ts.agentName;
          }
        }
        // A terminal phase (idle/failed) for the active conversation ends
        // the streamed turn: the next turn's items start a fresh turn.
        if (phase != TurnPhase.working &&
            ts.conversationId == _activeConvId) {
          final turn = _streamingTurn;
          refetchForTool = turn != null && streamedTurnHasToolCall(turn.parts);
          _streamingTurn = null;
        }
      });
      _refreshDeadman();
      // Pull the tool result(s) in from history so the output card appears
      // inline without the operator reopening the conversation.
      final convId = _activeConvId;
      if (refetchForTool && convId != null) {
        unawaited(_hydrateHistory(convId));
      }
      return;
    }
    // Agent-initiated client tool dispatch.
    if (ev.hasServerRequest()) {
      final req = ev.serverRequest;
      _session?.handleServerRequest(req);
      // Convenience: on RevealPath, pop open the endDrawer when one
      // exists so the user sees the navigation result.
      if (req.method == 'RevealPath') {
        _scaffoldKey.currentState?.openEndDrawer();
      } else if (req.method == 'RequestUserInput' ||
          req.method == 'RequestUserAuth') {
        _handleHitlRequest(req);
      }
      return;
    }
    // Streamed activity frames (typed text + tool calls) produced during
    // the turn. Demultiplex into the active assistant turn's typed parts,
    // keyed by item id. Filtered to the conversation being viewed.
    if (ev.hasStreamItem()) {
      _onStreamItem(ev.streamItem);
      return;
    }
    if (!ev.hasSendMessage()) return;
    final send = ev.sendMessage;
    // Conversation filter: only render assistant bubbles for the
    // conversation the user is currently viewing. Replies for other
    // conversations are dropped silently — the next time the user
    // switches to that conversation, history fetch will load them.
    if (send.conversationId.isEmpty) return;
    if (send.conversationId != _activeConvId) return;
    // If this turn already streamed typed parts, they are the source of
    // truth — drop the redundant terminal reply text (turn_state IDLE
    // still finalizes the turn).
    if (_streamingTurn != null && _streamingTurn!.hasParts) return;
    final text = send.content
        .where((b) => b.hasText())
        .map((b) => b.text.text)
        .join();
    if (text.isEmpty) return;
    setState(() {
      _turns.add(_Turn(role: _Role.assistant, text: text));
    });
    _scrollToBottom();
  }

  /// Route one streamed item frame to its assistant turn's typed parts.
  /// `ItemStart` appends a new part keyed by item id (unknown kind ignored,
  /// never throws); `ItemDelta` appends to the matching part's buffer;
  /// `ItemStop` is a no-op (the terminal turn_state finalizes the turn).
  void _onStreamItem(StreamItem item) {
    // Sub-agent frames carry a parent link pointing at the active
    // conversation; route them into their collapsible group instead of the
    // top-level turn. A frame with no parent link falls through to the normal
    // path below (its own conversation_id is the filter).
    if (item.parentConversationId == _activeConvId &&
        item.parentConversationId.isNotEmpty) {
      final groups = _subagentGroups ??=
          SubagentGroups(parentConversationId: _activeConvId!);
      setState(() {
        _ensureStreamingTurn();
        groups.apply(item);
        // The chip reflects the running sub-agent while its frames stream;
        // turn-idle clears it via the existing resets.
        if (item.agentName.isNotEmpty) _activeAgentName = item.agentName;
      });
      _scrollToBottom();
      return;
    }
    if (item.conversationId.isEmpty) return;
    if (item.conversationId != _activeConvId) return;

    if (item.hasStart()) {
      setState(() {
        final turn = _ensureStreamingTurn();
        // Unknown item kind is ignored inside applyStart (no throw). Roll
        // back the just-created empty turn if nothing was appended.
        final added = turn.parts.applyStart(item.itemId, item.start);
        if (!added && turn.parts.isEmpty && identical(_turns.last, turn)) {
          _turns.removeLast();
          _streamingTurn = null;
        }
      });
      _scrollToBottom();
      return;
    }

    if (item.hasDelta()) {
      final turn = _streamingTurn;
      if (turn == null) return;
      setState(() {
        turn.parts.applyDelta(item.itemId, item.delta);
      });
      return;
    }
    // ItemStop: no-op. The turn is finalized by the terminal turn_state.
  }

  /// The assistant turn currently accepting streamed parts, creating and
  /// appending a fresh one on the first item of a turn.
  _Turn _ensureStreamingTurn() {
    var turn = _streamingTurn;
    if (turn == null) {
      turn = _Turn(role: _Role.assistant);
      _turns.add(turn);
      _streamingTurn = turn;
    }
    return turn;
  }

  /// Render a HITL server-request (`RequestUserInput` / `RequestUserAuth`)
  /// as a modal, capture the user's answer, and echo it back as a
  /// `ClientResponse` keyed by the same `request_id`. A client-side timer
  /// (~30s, near the server's request cap) dismisses the prompt without
  /// answering — a late response is silently dropped server-side.
  Future<void> _handleHitlRequest(ServerRequest req) async {
    Map<String, dynamic> params;
    try {
      params = req.paramsJson.isEmpty
          ? <String, dynamic>{}
          : (jsonDecode(req.paramsJson) as Map<String, dynamic>);
    } catch (_) {
      params = <String, dynamic>{};
    }

    final timeout = Completer<String?>();
    final timer = Timer(const Duration(seconds: 30), () {
      if (!timeout.isCompleted) timeout.complete(null);
    });

    final answer = await showDialog<String>(
      context: _scaffoldKey.currentContext ?? context,
      builder: (ctx) => _HitlDialog(
        method: req.method,
        params: params,
        onTimeout: timeout.future,
      ),
    );
    timer.cancel();

    // A dialog dismissed by the timeout (or barrier tap) returns null: drop
    // it, matching the server's silent-drop of a late response.
    if (answer == null) return;
    await _sendClientResponse(req.requestId, answer);
  }

  /// Send a `ClientResponse` (the HITL answer) back over `ChannelIngest`,
  /// mutually exclusive with `user_message`. Echoes `request_id` verbatim so
  /// the awaiting server request resolves.
  Future<void> _sendClientResponse(String requestId, String resultJson) async {
    final channelId = _channelId;
    if (channelId == null) return;
    final client = RelayGatewayClient(_channel!);
    final req = ChannelIngestRequest()
      ..channelId = channelId
      ..supportedMethods.addAll(deviceRenderableMethods)
      ..conversationId = _activeConvId ?? ''
      ..clientResponse = (ClientResponse()
        ..requestId = requestId
        ..resultJson = resultJson);
    try {
      final sig = buildSignedMetadata(
        method: RelayMethods.channelIngest,
        protobufBytes: Uint8List.fromList(req.writeToBuffer()),
        workspace: widget.creds.workspace,
        clientName: widget.creds.clientName,
        keyPair: widget.creds.keyPair,
      );
      await client.channelIngest(
        req,
        options: CallOptions(metadata: sig.toMetadata()),
      );
    } catch (e) {
      _showErrorSnack('Could not send response: $e');
    }
  }

  /// Surface a SnackBar for a receive-stream error (transient →
  /// reconnecting toast; fatal → persistent "sign out" prompt). The
  /// reconnector classifies and schedules the reconnect; this only handles
  /// UI state.
  ///
  /// Only a FATAL (auth) error clears the indicator — the client is dead
  /// until re-enrollment. A TRANSIENT blip deliberately KEEPS the current
  /// phase so a working turn still shows "Working…" across the gap; the
  /// reconnect reconcile-poll (and the periodic poll) settle it to the real
  /// state. Clearing on every transient error used to drop the indicator to
  /// idle and re-enable the composer mid-turn.
  void _onReceiveStatus({required bool fatal}) {
    if (!mounted) return;
    if (fatal) {
      setState(() {
        _reconciler.clearAll();
      });
      _refreshDeadman();
    }
    final msg = fatal
        ? 'Signature rejected — sign out and re-enroll.'
        : 'Receive stream error — reconnecting…';
    if (_lastReceiveError != msg) {
      _lastReceiveError = msg;
      _showErrorSnack(msg, persistent: fatal);
    }
  }

  void _showErrorSnack(String message, {bool persistent = false}) {
    final messenger =
        ScaffoldMessenger.of(_scaffoldKey.currentContext ?? context);
    Future.microtask(() {
      if (!mounted) return;
      messenger.showSnackBar(
        SnackBar(
          content: Text(message),
          duration:
              persistent ? const Duration(days: 1) : const Duration(seconds: 3),
          behavior: SnackBarBehavior.floating,
          action: persistent
              ? SnackBarAction(
                  label: 'Sign out',
                  onPressed: _confirmSignOut,
                )
              : null,
        ),
      );
    });
  }

  @override
  void dispose() {
    // Reconnector dispose cancels its timer + subscription, so no
    // queued reopen can fire on a disposed State.
    _reconnector?.dispose();
    _poller?.dispose();
    _deadman?.dispose();
    _session?.dispose();
    _channel?.shutdown();
    _inputCtrl.dispose();
    _scrollCtrl.dispose();
    super.dispose();
  }

  Future<void> _send() async {
    final text = _inputCtrl.text.trim();
    // Block only while a turn is actually in flight. `failed` (and `idle`)
    // allow a send: re-submitting from a failed turn is the retry path.
    if (text.isEmpty ||
        _activePhase == TurnPhase.sending ||
        _activePhase == TurnPhase.working) {
      return;
    }
    _inputCtrl.clear();

    final preMintKey = _activeConvId ?? TurnStateReconciler.preMintKey;
    setState(() {
      _turns.add(_Turn(role: _Role.user, text: text));
      // A fresh send dismisses a stale prompt-change banner and clears the
      // prior turn's sub-agent groups.
      _promptChanged = false;
      _subagentGroups = null;
      // applyPush always wins; setting `sending` also clears any prior
      // failure affordance (the phase is no longer `failed`).
      _reconciler.applyPush(_activeConvId, TurnPhase.sending);
    });
    _scrollToBottom();

    final channelId = _channelId;
    if (channelId == null) {
      setState(() {
        _turns.add(_Turn(
          role: _Role.assistant,
          text:
              '[channel not yet registered — wait for the receive stream to open.]',
        ));
        _reconciler.applyPush(_activeConvId, TurnPhase.idle);
      });
      return;
    }

    final client = RelayGatewayClient(_channel!);
    final req = ChannelIngestRequest()
      ..channelId = channelId
      ..supportedMethods.addAll(deviceRenderableMethods)
      ..conversationId = _activeConvId ?? ''
      ..userMessage = (UserMessage()
        ..sender = widget.creds.clientName
        ..content.add(
          ContentBlock()..text = (TextBlock()..text = text),
        )
        ..grants.addAll(_selectedGrants.entries.map(
          (e) => GrantSelection()
            ..toolset = e.key
            ..grant = e.value,
        )));

    try {
      final sig = buildSignedMetadata(
        method: RelayMethods.channelIngest,
        protobufBytes: Uint8List.fromList(req.writeToBuffer()),
        workspace: widget.creds.workspace,
        clientName: widget.creds.clientName,
        keyPair: widget.creds.keyPair,
      );
      final ack = await client.channelIngest(
        req,
        options: CallOptions(metadata: sig.toMetadata()),
      );
      // The controller authoritatively stamps the conversation id on
      // every ack — either echoing back the one we sent, or returning
      // a freshly minted one when we asked for a new thread. Adopt it.
      if (ack.conversationId.isNotEmpty &&
          ack.conversationId != _activeConvId) {
        final firstStamp = _activeConvId == null;
        setState(() {
          _activeConvId = ack.conversationId;
          // Carry the in-flight "sending" phase entry from the pre-mint
          // key to the freshly stamped real id.
          _reconciler.carry(preMintKey, ack.conversationId);
        });
        unawaited(StoredConversations.write(
          widget.creds.workspace,
          ack.conversationId,
        ));
        if (firstStamp) {
          setState(() => _drawerRefreshTick++);
        }
      }
      // Ack received. We deliberately do NOT flip the phase here —
      // wait for the cluster-pushed WORKING event so the indicator
      // label transitions in lockstep with the actual turn dispatch.
    } on GrpcError catch (e) {
      switch (sendFailureDisposition(e)) {
        case SendFailure.fatalAuth:
          // Auth rejection is a client-lifecycle failure, not an assistant
          // reply — same persistent sign-out prompt the receive path uses.
          _onReceiveStatus(fatal: true);
        case SendFailure.transport:
          setState(() {
            _turns.add(_Turn(
                role: _Role.assistant,
                text: '[gRPC error ${e.code}: ${e.message ?? '?'}]'));
            _reconciler.applyPush(_activeConvId, TurnPhase.idle);
          });
      }
    } catch (e) {
      setState(() {
        _turns.add(_Turn(role: _Role.assistant, text: '[transport error: $e]'));
        _reconciler.applyPush(_activeConvId, TurnPhase.idle);
      });
    }
  }

  /// Cancel the in-flight turn (local stop). Invokes `CancelTurn` for the
  /// active conversation; the pushed terminal `turn_cancelled` is the
  /// authoritative signal that flips the indicator back to idle.
  Future<void> _cancelActiveTurn() async {
    final session = _session;
    final convId = _activeConvId;
    if (session == null || convId == null) return;
    try {
      await session.cancelTurn(convId);
    } catch (e) {
      _showErrorSnack('Could not cancel: $e');
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

  Future<void> _startNewConversation() async {
    final session = _session;
    if (session == null) return;
    try {
      // Pre-mint so the new thread shows up in the drawer immediately.
      final newId = await session.mintConversation();
      if (!mounted) return;
      setState(() {
        _activeConvId = newId;
        _turns.clear();
        _streamingTurn = null;
        _subagentGroups = null;
        _activeAgentName = null;
        _promptChanged = false;
        _drawerRefreshTick++;
      });
      await StoredConversations.write(widget.creds.workspace, newId);
      _scaffoldKey.currentState?.closeDrawer();
    } catch (e) {
      _showErrorSnack('Could not start a new conversation: $e');
    }
  }

  void _onConversationDeleted(String convId) {
    // Drop any in-memory phase entry for the deleted id so a stale
    // "working" never resurfaces.
    _reconciler.forget(convId);
    if (convId != _activeConvId) return;
    // The user just deleted the conversation they were viewing.
    // Clear the chat surface and the persistent cursor; the user
    // can pick another from the drawer or start a new one.
    setState(() {
      _activeConvId = null;
      _turns.clear();
      _streamingTurn = null;
      _subagentGroups = null;
      _activeAgentName = null;
      _promptChanged = false;
    });
    unawaited(StoredConversations.clearWorkspace(widget.creds.workspace));
  }

  Future<void> _switchConversation(String convId) async {
    _scaffoldKey.currentState?.closeDrawer();
    if (convId == _activeConvId) return;
    setState(() {
      _activeConvId = convId;
      _turns.clear();
      _streamingTurn = null;
      _subagentGroups = null;
      _activeAgentName = null;
      _promptChanged = false;
    });
    // The active conversation changed — track the deadman against whatever
    // phase the newly-viewed conversation is in.
    _refreshDeadman();
    await StoredConversations.write(widget.creds.workspace, convId);
    await _hydrateHistory(convId);
  }

  /// Fetch the skill's markdown and send it as the user message. The file
  /// body is the instruction; the name alone carries none of it.
  Future<void> _onSkillTrigger(String name) async {
    final session = _session;
    if (session == null) return;
    String body;
    try {
      body = await fetchSkillBody(
        session,
        name,
        conversationId: _activeConvId ?? '',
      );
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not load /$name: $e')),
      );
      return;
    }
    if (!mounted || body.isEmpty) return;
    _inputCtrl.text = body;
    await _send();
  }

  @override
  Widget build(BuildContext context) {
    final width = MediaQuery.sizeOf(context).width;
    final isDesktop = width >= 720;
    final session = _session;
    final hasSession = session != null && _channelId != null;
    final browser = hasSession
        ? BrowserPane(
            key: _browserKey,
            session: session,
            conversationId: _activeConvId ?? '',
          )
        : const Center(child: Text('Waiting for channel registration…'));

    return Scaffold(
      key: _scaffoldKey,
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text('${widget.creds.workspace} @ ${widget.creds.serverHost}'),
            if (_activeAgentName != null && _activeAgentName!.isNotEmpty)
              Text(
                _activeAgentName!,
                style: Theme.of(context).textTheme.bodySmall?.copyWith(
                      color: Theme.of(context).colorScheme.onSurfaceVariant,
                    ),
              ),
          ],
        ),
        actions: [
          if (!isDesktop && hasSession)
            IconButton(
              icon: const Icon(Icons.folder_open),
              tooltip: 'Workspace browser',
              onPressed: () => _scaffoldKey.currentState?.openEndDrawer(),
            ),
          IconButton(
            icon: const Icon(Icons.logout),
            tooltip: 'Sign out',
            onPressed: _confirmSignOut,
          ),
        ],
      ),
      drawer: hasSession
          ? Drawer(
              width: math.min(width * 0.85, 320),
              child: ConversationsDrawer(
                session: session,
                activeConvId: _activeConvId,
                refreshTick: _drawerRefreshTick,
                onPick: _switchConversation,
                onNew: _startNewConversation,
                onDeleted: _onConversationDeleted,
              ),
            )
          : null,
      endDrawer: isDesktop
          ? null
          : Drawer(
              width: math.min(width * 0.85, 360),
              child: browser,
            ),
      body: Row(
        children: [
          Expanded(
            child: Column(
              children: [
                Expanded(
                  child: Builder(builder: (ctx) {
                    final groups = _subagentGroups;
                    final childIds =
                        groups?.childConversationIds.toList() ?? const [];
                    // Sub-agent groups render as a trailing item under the
                    // active turn: one collapsible tile per child conversation.
                    final extra = childIds.isEmpty ? 0 : 1;
                    return ListView.builder(
                      controller: _scrollCtrl,
                      padding: const EdgeInsets.all(12),
                      itemCount: _turns.length + extra,
                      itemBuilder: (ctx, i) {
                        if (i < _turns.length) {
                          return _TurnBubble(turn: _turns[i]);
                        }
                        return Align(
                          alignment: Alignment.centerLeft,
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              for (final childId in childIds)
                                SubagentGroupTile(
                                  childConversationId: childId,
                                  name: groups!.nameFor(childId),
                                  children: [
                                    AssistantPartsView(
                                      parts: groups.partsFor(childId),
                                    ),
                                  ],
                                ),
                            ],
                          ),
                        );
                      },
                    );
                  }),
                ),
                if (_promptChanged)
                  Container(
                    width: double.infinity,
                    color: Theme.of(context).colorScheme.tertiaryContainer,
                    padding:
                        const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
                    child: Row(
                      children: [
                        Icon(Icons.warning_amber,
                            size: 16,
                            color:
                                Theme.of(context).colorScheme.onTertiaryContainer),
                        const SizedBox(width: 8),
                        Expanded(
                          child: Text(
                            'The agent’s system prompt changed since the '
                            'last turn.',
                            style: Theme.of(context).textTheme.bodySmall,
                          ),
                        ),
                        IconButton(
                          icon: const Icon(Icons.close, size: 16),
                          onPressed: () =>
                              setState(() => _promptChanged = false),
                        ),
                      ],
                    ),
                  ),
                PendingIndicator(
                  phase: _activePhase,
                  failureReason: _activeFailureReason,
                ),
                if (_grantMenu.isNotEmpty)
                  // One chip per (toolset, grant). A selected chip rides on
                  // every send until toggled off; the model never chooses.
                  Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 8),
                    child: Align(
                      alignment: Alignment.centerLeft,
                      child: Wrap(
                        spacing: 6,
                        children: [
                          for (final tc in _grantMenu)
                            for (final grant in tc.grants)
                              FilterChip(
                                label: Text('${tc.toolset}: $grant'),
                                selected:
                                    _selectedGrants[tc.toolset] == grant,
                                onSelected: (on) => setState(() {
                                  if (on) {
                                    _selectedGrants[tc.toolset] = grant;
                                  } else {
                                    _selectedGrants.remove(tc.toolset);
                                  }
                                }),
                              ),
                        ],
                      ),
                    ),
                  ),
                SafeArea(
                  top: false,
                  child: Padding(
                    padding: const EdgeInsets.all(8),
                    child: Row(
                      children: [
                        if (hasSession)
                          CommandMenuButton(
                            session: session,
                            onTrigger: _onSkillTrigger,
                            conversationId: _activeConvId ?? '',
                          ),
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
                        if (_activePhase == TurnPhase.working)
                          FilledButton(
                            style: FilledButton.styleFrom(
                              backgroundColor:
                                  Theme.of(context).colorScheme.error,
                            ),
                            onPressed: _cancelActiveTurn,
                            child: const Icon(Icons.stop),
                          )
                        else
                          FilledButton(
                            onPressed: (_activePhase == TurnPhase.idle ||
                                    _activePhase == TurnPhase.failed)
                                ? _send
                                : null,
                            child: const Icon(Icons.send),
                          ),
                      ],
                    ),
                  ),
                ),
              ],
            ),
          ),
          if (isDesktop) ...[
            const VerticalDivider(width: 1),
            SizedBox(width: 320, child: browser),
          ],
        ],
      ),
    );
  }
}

enum _Role { user, assistant, tool }

/// One rendered turn. User echoes and hydrated history use the flat [text]
/// path. A live-streamed assistant turn instead accumulates typed [parts]
/// (streamed text runs + tool calls); when non-empty they are the source of
/// truth and [text] is ignored. A [_Role.tool] turn is a completed tool call
/// rebuilt from history: [toolName]/[toolInput] paired from the assistant's
/// call and [toolOutput] the scrubbed result.
class _Turn {
  _Turn({
    required this.role,
    this.text = '',
    this.toolName = '',
    this.toolInput = '',
    this.toolOutput = '',
  });
  final _Role role;
  String text;

  /// Set only for a [_Role.tool] turn.
  final String toolName;
  final String toolInput;
  final String toolOutput;

  /// Typed parts for a streamed assistant turn. Empty for flat turns.
  final StreamedParts parts = StreamedParts();

  bool get hasParts => parts.isNotEmpty;
}

/// The concatenated text of a message's content blocks.
String _messageText(Message msg) =>
    msg.content.where((b) => b.hasText()).map((b) => b.text.text).join();

/// Rebuild the rendered turns from conversation history. User and assistant
/// messages map to flat text turns as before. A `tool`-role message carries
/// the (already-scrubbed) tool result; it is paired by `tool_call_id` to the
/// most recent assistant message's tool calls to recover the call's name and
/// input, and emitted as a [_Role.tool] turn so the output renders on screen.
List<_Turn> _turnsFromHistory(List<HistoryEntry> entries) {
  final turns = <_Turn>[];
  // Tool calls announced by the last assistant message, keyed by id.
  var pendingCalls = <String, ToolCall>{};
  for (final e in entries) {
    final msg = e.message;
    switch (msg.role) {
      case 'assistant':
        pendingCalls = {for (final c in msg.toolCalls) c.id: c};
        final text = _messageText(msg);
        if (text.trim().isNotEmpty) {
          turns.add(_Turn(role: _Role.assistant, text: text));
        }
      case 'user':
        final text = _messageText(msg);
        if (text.trim().isNotEmpty) {
          turns.add(_Turn(role: _Role.user, text: text));
        }
      case 'tool':
        final call = pendingCalls[msg.toolCallId];
        turns.add(_Turn(
          role: _Role.tool,
          toolName: call?.name ?? 'tool',
          toolInput: call?.inputJson ?? '',
          toolOutput: _messageText(msg),
        ));
    }
  }
  return turns;
}

/// Render conversation history as a list of turn bubbles. Composes the two
/// production units — [_turnsFromHistory] and [_TurnBubble] — for tests. The
/// live chat builds its bubbles lazily by index instead.
@visibleForTesting
List<Widget> transcriptBubblesFromHistory(List<HistoryEntry> entries) =>
    [for (final t in _turnsFromHistory(entries)) _TurnBubble(turn: t)];

/// Whether a just-completed streamed turn made a tool call. Gates the
/// history refetch that pulls in the tool's result card — text-only turns
/// skip it to avoid needless flicker and network.
@visibleForTesting
bool streamedTurnHasToolCall(StreamedParts parts) =>
    parts.parts.any((p) => p is ToolPart);

/// Cluster-aware turn indicator. Renders between the scrollable turn list
/// and the composer. Shows nothing while idle, so the chat is visually
/// unchanged when no turn is in flight. Sending is client-derived; Working
/// and Failed are pushed from the controller via the `TurnStateEvent`
/// frames on `ChannelReceive`. Failed shows the reason (no spinner) — the
/// State re-enables the composer so a resend retries.
@visibleForTesting
class PendingIndicator extends StatelessWidget {
  const PendingIndicator({super.key, required this.phase, this.failureReason});
  final TurnPhase phase;

  /// Human-readable failure reason, shown only when [phase] is `failed`.
  final String? failureReason;

  @override
  Widget build(BuildContext context) {
    if (phase == TurnPhase.idle) return const SizedBox.shrink();
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    if (phase == TurnPhase.failed) {
      return Padding(
        padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(Icons.error_outline, size: 16, color: scheme.error),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                failureReason ?? 'The turn failed. Send again to retry.',
                style: theme.textTheme.bodySmall?.copyWith(color: scheme.error),
              ),
            ),
          ],
        ),
      );
    }

    final label = phase == TurnPhase.sending ? 'Sending…' : 'Working…';
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
      child: Row(
        children: [
          SizedBox(
            width: 14,
            height: 14,
            child: CircularProgressIndicator(
              strokeWidth: 2,
              color: scheme.primary,
            ),
          ),
          const SizedBox(width: 10),
          Text(
            label,
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}

/// Modal that renders a HITL server-request and returns the answer JSON, or
/// null when dismissed (timeout / barrier). `RequestUserInput` renders the
/// prompt plus one button per action, returning [hitlInputResult] for the
/// chosen action. `RequestUserAuth` renders the authorization URL and a
/// single confirm button, returning [hitlAuthResult] once the user reports
/// the external callback resolved.
class _HitlDialog extends StatefulWidget {
  const _HitlDialog({
    required this.method,
    required this.params,
    required this.onTimeout,
  });

  final String method;
  final Map<String, dynamic> params;
  final Future<String?> onTimeout;

  @override
  State<_HitlDialog> createState() => _HitlDialogState();
}

class _HitlDialogState extends State<_HitlDialog> {
  @override
  void initState() {
    super.initState();
    // Auto-dismiss when the client-side timeout fires.
    widget.onTimeout.then((_) {
      if (mounted) Navigator.of(context).maybePop();
    });
  }

  @override
  Widget build(BuildContext context) {
    if (widget.method == 'RequestUserAuth') {
      final url = (widget.params['url'] ??
              widget.params['auth_url'] ??
              widget.params['authorization_url'] ??
              '')
          .toString();
      return AlertDialog(
        title: const Text('Authorization required'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text('Open this URL to authorize, then confirm:'),
            const SizedBox(height: 8),
            SelectableText(url),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, hitlAuthResult()),
            child: const Text('Done'),
          ),
        ],
      );
    }

    // RequestUserInput
    final prompt = (widget.params['prompt'] ?? '').toString();
    final rawActions = widget.params['actions'];
    final actions = <Map<String, dynamic>>[
      if (rawActions is List)
        for (final a in rawActions)
          if (a is Map<String, dynamic>) a,
    ];
    return AlertDialog(
      title: const Text('Input requested'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (prompt.isNotEmpty) Text(prompt),
          if (actions.isEmpty)
            const Padding(
              padding: EdgeInsets.only(top: 8),
              child: Text('(no actions offered)'),
            ),
        ],
      ),
      actions: [
        for (final action in actions)
          FilledButton(
            onPressed: () => Navigator.pop(
              context,
              hitlInputResult(
                (action['action_id'] ?? action['id'] ?? '').toString(),
                action['arguments'] as Map<String, dynamic>?,
              ),
            ),
            child: Text(
              (action['label'] ?? action['action_id'] ?? action['id'] ?? '?')
                  .toString(),
            ),
          ),
      ],
    );
  }
}

class _TurnBubble extends StatelessWidget {
  const _TurnBubble({required this.turn});
  final _Turn turn;

  @override
  Widget build(BuildContext context) {
    // A completed tool call renders as its own left-aligned card, not a
    // chat bubble — the result (e.g. a redaction proof) is evidence, not
    // conversation prose.
    if (turn.role == _Role.tool) {
      return Align(
        alignment: Alignment.centerLeft,
        child: ToolCallCard(
          name: turn.toolName,
          input: turn.toolInput,
          output: turn.toolOutput,
        ),
      );
    }
    final isUser = turn.role == _Role.user;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final bg = isUser ? scheme.primaryContainer : scheme.surfaceContainerHighest;
    return Align(
      alignment: isUser ? Alignment.centerRight : Alignment.centerLeft,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 4),
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: bg,
          borderRadius: BorderRadius.circular(12),
        ),
        constraints: isUser ? const BoxConstraints(maxWidth: 320) : null,
        child: _buildBody(theme, isUser),
      ),
    );
  }

  Widget _buildBody(ThemeData theme, bool isUser) {
    // A streamed assistant turn renders its typed parts (text runs +
    // tool calls) as distinct elements. Flat turns (user echoes, hydrated
    // history) render their single text blob as before.
    if (turn.hasParts) {
      return SelectionArea(child: AssistantPartsView(parts: turn.parts));
    }
    final displayText = turn.text.isEmpty ? '...' : turn.text;
    return isUser
        ? SelectableText(displayText)
        : SelectionArea(
            child: GptMarkdown(
              displayText,
              style: theme.textTheme.bodyMedium,
              onLinkTap: (url, title) {},
              highlightBuilder: _inlineCodeBuilder(theme),
              codeBuilder: _fencedCodeBuilder(theme),
              tableBuilder: _assistantTableBuilder(theme),
            ),
          );
  }
}

/// Renders a streamed assistant turn's typed parts as distinct elements:
/// text runs via [GptMarkdown], tool calls via [ToolCallCard]. Each part is
/// a separate widget so text and tool activity are visually distinct rather
/// than a single flat block.
@visibleForTesting
class AssistantPartsView extends StatelessWidget {
  const AssistantPartsView({super.key, required this.parts});
  final StreamedParts parts;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        for (final part in parts.parts) _buildPart(theme, part),
      ],
    );
  }

  Widget _buildPart(ThemeData theme, TurnPart part) {
    if (part is TextPart) {
      final text = part.text.toString();
      if (text.isEmpty) return const SizedBox.shrink();
      return GptMarkdown(
        text,
        style: theme.textTheme.bodyMedium,
        onLinkTap: (url, title) {},
        highlightBuilder: _inlineCodeBuilder(theme),
        codeBuilder: _fencedCodeBuilder(theme),
        tableBuilder: _assistantTableBuilder(theme),
      );
    }
    part as ToolPart;
    return ToolCallCard(name: part.name, input: part.input.toString());
  }
}

/// A distinct labeled element for a tool call: the tool name, its (possibly
/// partial) JSON arguments, and — once the result lands in history — the
/// tool's scrubbed output. Visually separated from prose.
@visibleForTesting
class ToolCallCard extends StatelessWidget {
  const ToolCallCard({
    super.key,
    required this.name,
    required this.input,
    this.output = '',
  });
  final String name;
  final String input;

  /// The tool's result text, shown once available. Empty while a call is
  /// still streaming (the live stream carries no result).
  final String output;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      margin: const EdgeInsets.symmetric(vertical: 4),
      padding: const EdgeInsets.all(8),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(Icons.build_outlined, size: 14, color: scheme.primary),
              const SizedBox(width: 6),
              Text(
                'tool: $name',
                style: theme.textTheme.labelMedium?.copyWith(
                  color: scheme.primary,
                  fontWeight: FontWeight.w600,
                ),
              ),
            ],
          ),
          if (input.isNotEmpty) ...[
            const SizedBox(height: 4),
            Text(
              input,
              style: theme.textTheme.bodySmall?.copyWith(
                fontFamily: 'monospace',
                color: scheme.onSurfaceVariant,
              ),
            ),
          ],
          if (output.isNotEmpty) ...[
            const SizedBox(height: 4),
            Text(
              'output: $output',
              style: theme.textTheme.bodySmall?.copyWith(
                fontFamily: 'monospace',
                color: scheme.onSurfaceVariant,
              ),
            ),
          ],
        ],
      ),
    );
  }
}

/// Inline ` `code` ` chip — monospace text on a surfaceContainer pill.
/// Overrides gpt_markdown's default bold+Paint-background highlight by
/// resetting the supplied style before rendering.
HighlightBuilder _inlineCodeBuilder(ThemeData theme) {
  final scheme = theme.colorScheme;
  return (BuildContext context, String text, TextStyle style) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 1),
      decoration: BoxDecoration(
        color: scheme.surfaceContainer,
        borderRadius: BorderRadius.circular(3),
      ),
      child: Text(
        text,
        style: style.copyWith(
          fontFamily: 'monospace',
          color: scheme.onSurfaceVariant,
          fontWeight: FontWeight.normal,
          background: null,
        ),
      ),
    );
  };
}

/// Fenced ``` ``` ``` block. Replaces gpt_markdown's bundled CodeField
/// (Material card + copy button + JetBrainsMono asset) with a minimal
/// surfaceContainer block matching the prior flutter_markdown look.
CodeBlockBuilder _fencedCodeBuilder(ThemeData theme) {
  final scheme = theme.colorScheme;
  return (BuildContext context, String name, String code, bool closed) {
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.symmetric(vertical: 4),
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: scheme.surfaceContainer,
        borderRadius: BorderRadius.circular(6),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: SelectableText(
        code,
        style: TextStyle(
          fontFamily: 'monospace',
          color: scheme.onSurfaceVariant,
        ),
      ),
    );
  };
}

/// HTML-auto-like column sizing for GFM tables. Flutter's `Table`
/// otherwise gives every column equal width (FlexColumnWidth default) or
/// uses IntrinsicColumnWidth (which forces horizontal scroll on prose).
/// We approximate browser auto-layout by weighting each column with
/// `FlexColumnWidth(sqrt(maxChars))` — sqrt damps the effect so one
/// runaway cell doesn't starve the others. Cell content is raw
/// markdown, so each cell renders recursively through GptMarkdown to
/// preserve inline `code`, **bold**, etc.
TableBuilder _assistantTableBuilder(ThemeData theme) {
  final scheme = theme.colorScheme;
  final headStyle = theme.textTheme.bodyMedium?.copyWith(
    fontWeight: FontWeight.w600,
  );

  return (
    BuildContext context,
    List<CustomTableRow> rows,
    TextStyle textStyle,
    GptMarkdownConfig config,
  ) {
    if (rows.isEmpty) return const SizedBox.shrink();
    final columnCount = rows
        .map((r) => r.fields.length)
        .fold<int>(0, math.max);
    if (columnCount == 0) return const SizedBox.shrink();

    final columnWidths = <int, TableColumnWidth>{};
    for (var col = 0; col < columnCount; col++) {
      var maxChars = 1;
      for (final row in rows) {
        if (col >= row.fields.length) continue;
        final len = row.fields[col].data.trim().length;
        if (len > maxChars) maxChars = len;
      }
      final weight = math.sqrt(maxChars.clamp(1, 400).toDouble());
      columnWidths[col] = FlexColumnWidth(weight);
    }

    TableRow buildRow(CustomTableRow row) {
      final cellStyle = row.isHeader ? headStyle : textStyle;
      return TableRow(
        decoration: row.isHeader
            ? BoxDecoration(color: scheme.surfaceContainerHigh)
            : null,
        children: List.generate(columnCount, (col) {
          final field = col < row.fields.length
              ? row.fields[col]
              : CustomTableField(data: '', alignment: TextAlign.left);
          return TableCell(
            verticalAlignment: TableCellVerticalAlignment.middle,
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: 8,
                vertical: 6,
              ),
              child: Align(
                alignment: switch (field.alignment) {
                  TextAlign.center => Alignment.center,
                  TextAlign.right => Alignment.centerRight,
                  _ => Alignment.centerLeft,
                },
                child: GptMarkdown(
                  field.data.trim(),
                  style: cellStyle,
                  textAlign: field.alignment,
                  highlightBuilder: config.highlightBuilder,
                  codeBuilder: config.codeBuilder,
                ),
              ),
            ),
          );
        }),
      );
    }

    return Table(
      columnWidths: columnWidths,
      defaultVerticalAlignment: TableCellVerticalAlignment.middle,
      border: TableBorder.all(color: scheme.outlineVariant),
      children: rows.map(buildRow).toList(),
    );
  };
}

(String, int)? _parseHostPort(String input) {
  final i = input.lastIndexOf(':');
  if (i <= 0 || i == input.length - 1) return null;
  final host = input.substring(0, i);
  final port = int.tryParse(input.substring(i + 1));
  if (port == null || port < 1 || port > 65535) return null;
  return (host, port);
}
