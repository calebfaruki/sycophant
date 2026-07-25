import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';

import 'agent_session.dart';
import 'content_parts.dart';
import 'generated/sycophant/common/v1/common.pb.dart';

/// Chamber tool invoked to render a file's preview. Returns a content-part
/// list; a previewable file (e.g. a PDF page) comes back as an image part.
/// The browser stays tool-agnostic — it renders whatever image part the tool
/// returns, regardless of which chamber implements the preview.
const _previewToolName = 'Preview';

/// Read-only workspace browser. Tap a directory to descend; tap a
/// breadcrumb segment to jump back up; the back chevron walks one level.
/// Listings come from the `Search` tool — same surface the agent uses.
class BrowserPane extends StatefulWidget {
  const BrowserPane({super.key, required this.session});
  final AgentSession session;

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
    final resp = await widget.session.callTool('Search', input);
    final text = joinTextParts(resp.content);
    if (resp.isError) {
      throw Exception(text);
    }
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

  /// Preview a file tap: invoke the chamber's preview tool for the file's
  /// path, walk the returned content parts, and show any image part in a
  /// full-screen overlay. Text-only answers show nothing inline in the row.
  Future<void> _previewFile(List<String> segs) async {
    final path = _absPath(segs);
    final input = jsonEncode({'path': path});
    final CallToolResponse resp;
    try {
      resp = await widget.session.callTool(_previewToolName, input);
    } catch (e) {
      // Preview is best-effort: show nothing on failure, but record why.
      debugPrint('Preview tool call failed for $path: $e');
      return;
    }
    if (!mounted) return;
    final image = firstImagePart(resp.content);
    if (image == null) return;
    await _showImageOverlay(image);
  }

  Future<void> _showImageOverlay(ImageBlock image) {
    final bytes = Uint8List.fromList(image.data);
    return showDialog<void>(
      context: context,
      barrierColor: Colors.black,
      builder: (ctx) => Dialog.fullscreen(
        backgroundColor: Colors.black,
        child: Stack(
          children: [
            Positioned.fill(
              child: InteractiveViewer(
                child: Center(child: Image.memory(bytes)),
              ),
            ),
            SafeArea(
              child: Align(
                alignment: Alignment.topLeft,
                child: IconButton(
                  icon: const Icon(Icons.close, color: Colors.white),
                  onPressed: () => Navigator.of(ctx).pop(),
                  tooltip: 'Close',
                ),
              ),
            ),
          ],
        ),
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
