import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';

import 'agent_session.dart';
import 'generated/sycophant/common/v1/common.pb.dart';

/// Toolset tool invoked to render a file's preview. Returns a content-part
/// list; a previewable file (e.g. a PDF page) comes back as an image part.
/// The browser stays tool-agnostic — it renders whatever image part the tool
/// returns, regardless of which toolset implements the preview.
const _previewToolName = 'Preview';

/// Phase of a client-driven tool call the browser pane owns locally: idle
/// before dispatch, pending once the server-minted call_id is known, then
/// resolved or failed on the terminal outcome. DONE and CANCELED both resolve
/// without an error; only FAILED surfaces one.
enum ToolCallPhase { idle, pending, resolved, failed }

/// Map a tool call's terminal outcome to its phase. DONE and CANCELED collapse
/// to one non-error resolved phase; only FAILED is the failed phase — a
/// user-initiated cancel is terminal but never shown as an error.
@visibleForTesting
ToolCallPhase toolCallPhaseFromOutcome(ToolOutcome outcome) {
  if (outcome == ToolOutcome.TOOL_OUTCOME_FAILED) return ToolCallPhase.failed;
  // DONE, CANCELED (and any unspecified terminal) resolve without an error.
  return ToolCallPhase.resolved;
}

/// Single-writer owner of one client-driven tool call's lifecycle, held on
/// `BrowserPaneState`. The client owns this locally — there is no
/// cluster-pushed turn state for a dispatched tool call. Dispatch enters
/// pending on the server-minted call_id; the interrupt is offered only
/// while pending; the terminal outcome clears pending.
@visibleForTesting
class ToolCallReconciler {
  ToolCallPhase _phase = ToolCallPhase.idle;
  String? _pendingCallId;

  ToolCallPhase get phase => _phase;

  /// The pending call's server-minted call_id, retained so the interrupt can
  /// cancel THAT call by id. Null unless a call is pending.
  String? get pendingCallId => _pendingCallId;

  /// The interrupt affordance is offered only while a call is pending.
  bool get canInterrupt => _phase == ToolCallPhase.pending;

  /// Enter pending on receipt of the server-minted call_id.
  void dispatch(String callId) {
    _pendingCallId = callId;
    _phase = ToolCallPhase.pending;
  }

  /// Resolve on the terminal outcome, clearing pending.
  void applyOutcome(ToolOutcome outcome) {
    _phase = toolCallPhaseFromOutcome(outcome);
    _pendingCallId = null;
  }
}

/// Read-only workspace browser. Tap a directory to descend; tap a
/// breadcrumb segment to jump back up; the back chevron walks one level.
/// Listings come from the `Search` tool — same surface the agent uses.
class BrowserPane extends StatefulWidget {
  const BrowserPane({
    super.key,
    required this.session,
    this.conversationId = '',
  });
  final AgentSession session;

  /// The active conversation a previewed-file dispatch attaches to, so the
  /// call's frames land in that conversation's execution log.
  final String conversationId;

  @override
  State<BrowserPane> createState() => BrowserPaneState();
}

class BrowserPaneState extends State<BrowserPane> {
  // Path stack as plain segments. `[]` means the workspace root.
  List<String> _segments = const [];
  Future<List<_Entry>>? _listing;
  int _reqId = 0;
  late final StreamSubscription _serverReqSub;

  @override
  void initState() {
    super.initState();
    _serverReqSub = widget.session.serverRequests.listen(_onServerRequest);
    _navigateTo(const []);
  }

  @override
  void dispose() {
    _serverReqSub.cancel();
    super.dispose();
  }

  void _onServerRequest(ServerRequest req) {
    if (req.method != 'RevealPath') return;
    try {
      final params = jsonDecode(req.paramsJson) as Map<String, dynamic>;
      final path = params['path'] as String?;
      if (path == null) return;
      revealPath(path);
    } catch (_) {
      // Malformed params: drop silently.
    }
  }

  /// Public hook: snap to an absolute path under /workspace. Called by
  /// the agent via RevealPath and by external code that wants to drive
  /// the browser.
  void revealPath(String absolutePath) {
    final trimmed = absolutePath
        .replaceAll(RegExp(r'^/workspace/?'), '')
        .replaceAll(RegExp(r'^/'), '');
    final segs = trimmed.isEmpty
        ? <String>[]
        : trimmed.split('/').where((s) => s.isNotEmpty).toList();
    _navigateTo(segs);
  }

  void _navigateTo(List<String> next) {
    final id = ++_reqId;
    final fetch = _listPath(next);
    setState(() {
      _segments = next;
      _listing = fetch.then((entries) {
        if (id != _reqId) throw _Stale();
        return entries;
      });
    });
  }

  /// The absolute workspace path for a breadcrumb segment list.
  String _absPath(List<String> segs) =>
      segs.isEmpty ? '/workspace' : '/workspace/${segs.join('/')}';

  Future<List<_Entry>> _listPath(List<String> segs) async {
    final path = _absPath(segs);
    final input = jsonEncode({
      'target': 'files',
      'pattern': '',
      'path': path,
    });
    final text = await callToolText(
      widget.session,
      'Search',
      input,
      conversationId: widget.conversationId,
    );
    // Search files-mode emits one path per line under `path`. Strip the
    // shared prefix so we render basenames; mark dirs by trailing
    // slash if the underlying tool emits one, otherwise treat all as
    // files for now.
    final lines = text
        .split('\n')
        .map((s) => s.trim())
        .where((s) => s.isNotEmpty)
        .toList();
    final prefix = path.endsWith('/') ? path : '$path/';
    return lines.map((raw) {
      // ripgrep emits paths relative to the working directory; strip
      // workspace prefix if present, then the current-path prefix.
      var name = raw;
      if (name.startsWith(prefix)) {
        name = name.substring(prefix.length);
      }
      // Only show top-level entries (no nested paths in this view).
      // Anything containing '/' descends deeper than this level.
      final slash = name.indexOf('/');
      final isDir = slash >= 0;
      final display = isDir ? name.substring(0, slash) : name;
      return _Entry(display, isDir);
    }).fold<List<_Entry>>(
      <_Entry>[],
      (acc, e) {
        if (!acc.any((x) => x.name == e.name)) {
          acc.add(e);
        } else if (e.isDir) {
          // Promote to dir if a deeper match revealed it.
          final i = acc.indexWhere((x) => x.name == e.name);
          acc[i] = _Entry(e.name, true);
        }
        return acc;
      },
    )..sort((a, b) {
        if (a.isDir != b.isDir) return a.isDir ? -1 : 1;
        return a.name.compareTo(b.name);
      });
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Column(
      children: [
        // Header: back chevron + breadcrumb.
        Container(
          height: 44,
          decoration: BoxDecoration(
            color: scheme.surface,
            border: Border(
              bottom: BorderSide(color: scheme.outlineVariant, width: 1),
            ),
          ),
          child: Row(
            children: [
              IconButton(
                icon: const Icon(Icons.chevron_left),
                onPressed: _segments.isEmpty
                    ? null
                    : () => _navigateTo(_segments.sublist(0, _segments.length - 1)),
                tooltip: 'Up one level',
              ),
              Expanded(
                child: SingleChildScrollView(
                  scrollDirection: Axis.horizontal,
                  reverse: true,
                  child: Row(
                    children: _breadcrumbSegments(scheme),
                  ),
                ),
              ),
              IconButton(
                icon: const Icon(Icons.refresh),
                onPressed: () => _navigateTo(_segments),
                tooltip: 'Refresh',
              ),
            ],
          ),
        ),
        Expanded(
          child: FutureBuilder<List<_Entry>>(
            future: _listing,
            builder: (context, snapshot) {
              if (snapshot.connectionState == ConnectionState.waiting) {
                return const Center(child: CircularProgressIndicator());
              }
              if (snapshot.hasError) {
                final err = snapshot.error;
                if (err is _Stale) return const SizedBox.shrink();
                return _ErrorView(
                  message: err.toString(),
                  onRetry: () => _navigateTo(_segments),
                );
              }
              final entries = snapshot.data ?? const <_Entry>[];
              if (entries.isEmpty) {
                return Center(
                  child: Text(
                    'This folder is empty.',
                    style: TextStyle(color: scheme.onSurfaceVariant),
                  ),
                );
              }
              return ListView.builder(
                itemCount: entries.length,
                itemBuilder: (context, i) {
                  final e = entries[i];
                  return ListTile(
                    leading: Icon(
                      e.isDir ? Icons.folder : Icons.insert_drive_file,
                      color: e.isDir ? scheme.primary : scheme.onSurfaceVariant,
                    ),
                    title: Text(e.name),
                    onTap: e.isDir
                        ? () => _navigateTo([..._segments, e.name])
                        : () => _previewFile([..._segments, e.name]),
                  );
                },
              );
            },
          ),
        ),
      ],
    );
  }

  /// Preview a file tap: dispatch the toolset's preview tool and open a
  /// full-screen overlay that owns the call's lifecycle — pending spinner with
  /// an interrupt affordance, the image rendered as its frame arrives, and a
  /// three-way terminal (a clean or canceled call clears pending with no
  /// banner; only a failure shows an error).
  Future<void> _previewFile(List<String> segs) async {
    final path = _absPath(segs);
    await showDialog<void>(
      context: context,
      barrierColor: Colors.black,
      builder: (ctx) => _PreviewOverlay(
        session: widget.session,
        path: path,
        conversationId: widget.conversationId,
      ),
    );
  }

  List<Widget> _breadcrumbSegments(ColorScheme scheme) {
    final widgets = <Widget>[
      TextButton(
        onPressed: _segments.isEmpty ? null : () => _navigateTo(const []),
        child: const Text('workspace'),
      ),
    ];
    for (var i = 0; i < _segments.length; i++) {
      widgets.add(Text(' / ', style: TextStyle(color: scheme.onSurfaceVariant)));
      final isLast = i == _segments.length - 1;
      widgets.add(
        TextButton(
          onPressed: isLast ? null : () => _navigateTo(_segments.sublist(0, i + 1)),
          child: Text(_segments[i]),
        ),
      );
    }
    return widgets;
  }
}

class _Entry {
  const _Entry(this.name, this.isDir);
  final String name;
  final bool isDir;
}

/// Full-screen preview overlay that owns one client-driven Preview call's
/// lifecycle. Dispatches on open, enters pending, streams the tool's
/// frames live, renders the image as it arrives, and settles on the
/// terminal: a clean or canceled call clears pending with no banner,
/// only a failure surfaces an error. While pending it offers an interrupt
/// that issues `CancelTool` for this call's id.
class _PreviewOverlay extends StatefulWidget {
  const _PreviewOverlay({
    required this.session,
    required this.path,
    required this.conversationId,
  });
  final AgentSession session;
  final String path;
  final String conversationId;

  @override
  State<_PreviewOverlay> createState() => _PreviewOverlayState();
}

class _PreviewOverlayState extends State<_PreviewOverlay> {
  final ToolCallReconciler _reconciler = ToolCallReconciler();
  StreamSubscription<ToolResultFrame>? _sub;
  ImageBlock? _image;
  final StringBuffer _stderr = StringBuffer();
  String? _error;

  @override
  void initState() {
    super.initState();
    _start();
  }

  Future<void> _start() async {
    final input = jsonEncode({'path': widget.path});
    final String callId;
    try {
      callId = await widget.session.dispatchTool(
        _previewToolName,
        input,
        conversationId: widget.conversationId,
      );
    } catch (e) {
      if (!mounted) return;
      setState(() => _error = 'Preview failed to start: $e');
      return;
    }
    if (!mounted) return;
    setState(() => _reconciler.dispatch(callId));
    _sub = widget.session
        .awaitToolResult(callId, conversationId: widget.conversationId)
        .listen(
          _onFrame,
          onError: _onStreamError,
        );
  }

  void _onFrame(ToolResultFrame frame) {
    if (!mounted) return;
    if (frame.hasImage()) {
      setState(() => _image = frame.image);
    } else if (frame.hasStderr()) {
      if (_stderr.isNotEmpty) _stderr.write('\n');
      _stderr.write(frame.stderr);
    } else if (frame.hasComplete()) {
      setState(() {
        _reconciler.applyOutcome(frame.complete.outcome);
        if (_reconciler.phase == ToolCallPhase.failed) {
          _error = _stderr.isNotEmpty
              ? _stderr.toString()
              : 'The preview failed.';
        }
      });
      // A clean or canceled call with no image to show has nothing to
      // display — dismiss rather than leave an empty overlay.
      if (_reconciler.phase == ToolCallPhase.resolved && _image == null) {
        Navigator.of(context).maybePop();
      }
    }
  }

  void _onStreamError(Object error) {
    if (!mounted) return;
    setState(() {
      _reconciler.applyOutcome(ToolOutcome.TOOL_OUTCOME_FAILED);
      _error = 'Preview stream error: $error';
    });
  }

  Future<void> _interrupt() async {
    final callId = _reconciler.pendingCallId;
    if (callId == null) return;
    // Best-effort: the terminal the runtime emits (CANCELED, or a result that
    // already finished) settles the overlay through the normal frame path.
    await widget.session.cancelTool(callId);
  }

  @override
  void dispose() {
    _sub?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Dialog.fullscreen(
      backgroundColor: Colors.black,
      child: Stack(
        children: [
          Positioned.fill(child: Center(child: _content())),
          SafeArea(
            child: Align(
              alignment: Alignment.topLeft,
              child: IconButton(
                icon: const Icon(Icons.close, color: Colors.white),
                onPressed: () => Navigator.of(context).maybePop(),
                tooltip: 'Close',
              ),
            ),
          ),
          if (_reconciler.canInterrupt)
            SafeArea(
              child: Align(
                alignment: Alignment.topRight,
                child: Padding(
                  padding: const EdgeInsets.all(8),
                  child: FilledButton.icon(
                    style: FilledButton.styleFrom(
                      backgroundColor: Colors.red,
                    ),
                    icon: const Icon(Icons.stop),
                    label: const Text('Stop'),
                    onPressed: _interrupt,
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }

  Widget _content() {
    if (_image != null) {
      return InteractiveViewer(
        child: Image.memory(Uint8List.fromList(_image!.data)),
      );
    }
    if (_error != null) {
      return Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.error_outline, color: Colors.white70, size: 32),
            const SizedBox(height: 8),
            Text(
              _error!,
              textAlign: TextAlign.center,
              style: const TextStyle(color: Colors.white70),
            ),
          ],
        ),
      );
    }
    // Pending: no image yet, no terminal.
    return const CircularProgressIndicator(color: Colors.white);
  }
}

class _Stale implements Exception {
  const _Stale();
}

class _ErrorView extends StatelessWidget {
  const _ErrorView({required this.message, required this.onRetry});
  final String message;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return SingleChildScrollView(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.center,
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.error_outline, color: scheme.error, size: 32),
          const SizedBox(height: 8),
          Text(
            message,
            textAlign: TextAlign.center,
            style: TextStyle(color: scheme.onSurfaceVariant),
          ),
          const SizedBox(height: 12),
          ElevatedButton(onPressed: onRetry, child: const Text('Retry')),
        ],
      ),
    );
  }
}
