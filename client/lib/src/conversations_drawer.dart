import 'dart:async';

import 'package:flutter/material.dart';

import 'agent_session.dart';
import 'generated/sycophant/common/v1/common.pb.dart';

/// Left-side drawer that lists the workspace's conversations and lets
/// the user start a new one or switch threads. Stateful so it can
/// refetch when the parent bumps `refreshTick` (e.g., after a fresh
/// conversation id is minted on first message).
class ConversationsDrawer extends StatefulWidget {
  const ConversationsDrawer({
    super.key,
    required this.session,
    required this.activeConvId,
    required this.refreshTick,
    required this.onPick,
    required this.onNew,
    required this.onDeleted,
  });

  final AgentSession session;
  final String? activeConvId;
  final int refreshTick;
  final ValueChanged<String> onPick;

  /// Called when the user taps "+ New conversation". The handler should
  /// pre-mint via `AgentSession.mintConversation()`, set the new id as
  /// the active one, and clear the chat list. Drawer closes
  /// automatically after the callback returns.
  final Future<void> Function() onNew;

  /// Called after a successful DeleteConversation RPC. Parent must
  /// clear active state if the deleted id was the active one and
  /// reset any in-memory phase entries for it.
  final ValueChanged<String> onDeleted;

  @override
  State<ConversationsDrawer> createState() => _ConversationsDrawerState();
}

class _ConversationsDrawerState extends State<ConversationsDrawer> {
  List<ConversationSummary>? _summaries;
  String? _error;
  int _seenTick = -1;
  int _reqId = 0;

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  @override
  void didUpdateWidget(covariant ConversationsDrawer old) {
    super.didUpdateWidget(old);
    if (widget.refreshTick != _seenTick) {
      _refresh();
    }
  }

  Future<void> _refresh() async {
    _seenTick = widget.refreshTick;
    final id = ++_reqId;
    try {
      final s = await widget.session.listConversations();
      if (!mounted || id != _reqId) return;
      setState(() {
        _summaries = s;
        _error = null;
      });
    } catch (e) {
      if (!mounted || id != _reqId) return;
      setState(() => _error = e.toString());
    }
  }

  Future<void> _confirmDelete(ConversationSummary s) async {
    final convId = s.conversationId;
    final label = s.name.isNotEmpty ? s.name : convId;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Delete conversation?'),
        content: Text(
          'Permanently delete "$label"? Its messages will be removed '
          'from the server with no recovery.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(false),
            child: const Text('Cancel'),
          ),
          FilledButton.tonal(
            style: FilledButton.styleFrom(
              foregroundColor: Theme.of(ctx).colorScheme.onErrorContainer,
              backgroundColor: Theme.of(ctx).colorScheme.errorContainer,
            ),
            onPressed: () => Navigator.of(ctx).pop(true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (confirmed != true) return;
    try {
      await widget.session.deleteConversation(convId);
      if (!mounted) return;
      // Invalidate any in-flight `_refresh` so its pre-delete snapshot
      // can't land after us and resurrect the deleted row.
      ++_reqId;
      // Optimistically drop from the local list so the row vanishes
      // immediately; the next refresh confirms.
      setState(() {
        _summaries = _summaries
            ?.where((s) => s.conversationId != convId)
            .toList(growable: false);
      });
      widget.onDeleted(convId);
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Delete failed: $e')),
      );
    }
  }

  /// Show a rename dialog and commit the new name with an optimistic
  /// local update + rollback on RPC failure. The `_reqId` stamp prevents
  /// a slow failing rename from clobbering a faster successful rename
  /// that ran second.
  Future<void> _renameConversation(ConversationSummary s) async {
    final controller = TextEditingController(text: s.name);
    final picked = await showDialog<String>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Rename conversation'),
        content: TextField(
          controller: controller,
          autofocus: true,
          decoration: const InputDecoration(labelText: 'Name'),
          onSubmitted: (v) => Navigator.of(ctx).pop(v),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(null),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(ctx).pop(controller.text),
            child: const Text('Rename'),
          ),
        ],
      ),
    );
    if (!mounted || picked == null) return;
    final trimmed = picked.trim();
    // 200 is the server-side cap (MAX_CONVERSATION_NAME_CHARS in
    // relay-controller). Mirrored client-side so an over-long name
    // never gets the round-trip + optimistic-then-rollback churn.
    if (trimmed.isEmpty || trimmed == s.name || trimmed.length > 200) return;
    final id = ++_reqId;
    final previous = s.name;
    final convId = s.conversationId;
    _patchSummaryName(convId, trimmed);
    try {
      await widget.session.setConversationName(convId, trimmed);
      // Success: optimistic state already correct. Stale-stamp check is
      // unnecessary for the no-op success branch.
    } catch (e) {
      if (!mounted || id != _reqId) return;
      _patchSummaryName(convId, previous);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Rename failed: $e')),
      );
    }
  }

  /// Replace the `name` of one row in `_summaries` in-place. Used by the
  /// optimistic apply and the rollback paths in `_renameConversation`;
  /// keeping them as one helper preserves the symmetry that makes
  /// rollback correct.
  void _patchSummaryName(String convId, String newName) {
    setState(() {
      _summaries = _summaries
          ?.map((row) =>
              row.conversationId == convId ? (row.deepCopy()..name = newName) : row)
          .toList(growable: false);
    });
  }

  String _relative(int msEpoch) {
    if (msEpoch == 0) return 'new';
    final now = DateTime.now().millisecondsSinceEpoch;
    final delta = now - msEpoch;
    if (delta < 60 * 1000) return 'just now';
    if (delta < 60 * 60 * 1000) return '${delta ~/ (60 * 1000)}m ago';
    if (delta < 24 * 60 * 60 * 1000) return '${delta ~/ (60 * 60 * 1000)}h ago';
    return '${delta ~/ (24 * 60 * 60 * 1000)}d ago';
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return SafeArea(
      child: Column(
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 16, 16, 8),
            child: Align(
              alignment: Alignment.centerLeft,
              child: Text(
                'Conversations',
                style: Theme.of(context).textTheme.titleMedium,
              ),
            ),
          ),
          ListTile(
            leading: const Icon(Icons.add),
            title: const Text('New conversation'),
            onTap: () async {
              await widget.onNew();
              // The parent (_startNewConversation) is responsible for
              // closing the drawer via the scaffold key now — leaves
              // a single ownership for the close action.
            },
          ),
          const Divider(height: 1),
          Expanded(
            child: Builder(
              builder: (context) {
                if (_summaries == null && _error == null) {
                  // Static text instead of a spinner — the spinner's
                  // Ticker repaints every frame regardless of drawer
                  // visibility because Scaffold keeps the drawer
                  // mounted while closed. `_refresh` sets `_error` on
                  // failure, so this branch is bounded by the gRPC
                  // deadline.
                  return const Center(child: Text('Loading conversations…'));
                }
                if (_error != null) {
                  return Center(
                    child: Padding(
                      padding: const EdgeInsets.all(16),
                      child: TextButton.icon(
                        icon: const Icon(Icons.refresh),
                        label: Text(
                          'Failed to load — retry',
                          style: TextStyle(color: scheme.error),
                        ),
                        onPressed: _refresh,
                      ),
                    ),
                  );
                }
                final list = _summaries!;
                if (list.isEmpty) {
                  return Center(
                    child: Text(
                      'No conversations yet.',
                      style: TextStyle(color: scheme.onSurfaceVariant),
                    ),
                  );
                }
                return ListView.builder(
                  itemCount: list.length,
                  itemBuilder: (context, i) {
                    final s = list[i];
                    final selected = s.conversationId == widget.activeConvId;
                    return ListTile(
                      selected: selected,
                      leading: Icon(
                        Icons.chat_bubble_outline,
                        color: selected ? scheme.primary : null,
                      ),
                      title: Text(s.name),
                      subtitle: Text(
                        _relative(s.lastTouchedMsEpoch.toInt()),
                        style: TextStyle(color: scheme.onSurfaceVariant),
                      ),
                      trailing: Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          IconButton(
                            icon: const Icon(Icons.edit_outlined, size: 20),
                            tooltip: 'Rename conversation',
                            onPressed: () => _renameConversation(s),
                          ),
                          IconButton(
                            icon: const Icon(Icons.delete_outline, size: 20),
                            tooltip: 'Delete conversation',
                            onPressed: () => _confirmDelete(s),
                          ),
                        ],
                      ),
                      onTap: () {
                        widget.onPick(s.conversationId);
                      },
                    );
                  },
                );
              },
            ),
          ),
        ],
      ),
    );
  }
}
