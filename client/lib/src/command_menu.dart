import 'dart:convert';

import 'package:flutter/material.dart';

import 'agent_session.dart';
import 'content_parts.dart';
import 'generated/sycophant/common/v1/common.pb.dart';

/// One user-facing command in the slash menu: a skill name plus its
/// one-line description (the skill file's first paragraph, supplied by
/// the `Skills` tool's detail mode).
class Command {
  const Command(this.name, this.description);
  final String name;
  final String description;
}

/// Parse the `Skills` detail payload (`[{name, description}, ...]`) into
/// commands, dropping `_`-prefixed names — underscore is the convention
/// for an agent-internal reference, not a user-facing command. Exposed
/// for tests.
List<Command> parseCommands(String json) {
  return (jsonDecode(json) as List)
      .cast<Map<String, dynamic>>()
      .map((m) => Command(
            m['name'] as String,
            (m['description'] as String?) ?? '',
          ))
      .where((c) => !c.name.startsWith('_'))
      .toList();
}

/// Split a one-line description into inline runs for the menu subtitle:
/// each run carries its text and whether it was backtick-delimited (so it
/// renders monospace, like the chat bubbles' inline `code`). Splitting on
/// the backtick makes even segments plain prose and odd segments code.
/// Empty segments (adjacent or leading/trailing backticks) are dropped.
/// Exposed for tests.
List<({String text, bool code})> parseDescriptionSpans(String description) {
  final parts = description.split('`');
  final runs = <({String text, bool code})>[];
  for (var i = 0; i < parts.length; i++) {
    if (parts[i].isEmpty) continue;
    runs.add((text: parts[i], code: i.isOdd));
  }
  return runs;
}

/// A "/" button for the composer — a Telegram-style command menu. Tapping
/// it opens a bottom sheet of the workspace's skills with descriptions;
/// tapping one fires [onTrigger] with the skill name (the same path the
/// old top-of-screen skills row used: the name is sent as a user message
/// and the persona routes it).
class CommandMenuButton extends StatelessWidget {
  const CommandMenuButton({
    super.key,
    required this.session,
    required this.onTrigger,
    required this.conversationId,
  });

  final AgentSession session;
  final void Function(String skillName) onTrigger;

  /// The active conversation the Skills dispatch attaches to, so the call's
  /// frames land in that conversation's execution log.
  final String conversationId;

  @override
  Widget build(BuildContext context) {
    return IconButton(
      tooltip: 'Commands',
      icon: const Text(
        '/',
        style: TextStyle(fontSize: 22, fontWeight: FontWeight.w600),
      ),
      onPressed: () async {
        final picked = await showModalBottomSheet<String>(
          context: context,
          showDragHandle: true,
          builder: (_) =>
              _CommandSheet(session: session, conversationId: conversationId),
        );
        if (picked == null || !context.mounted) return;
        // Confirm before dispatching the command.
        final confirmed = await showDialog<bool>(
          context: context,
          builder: (ctx) => AlertDialog(
            title: Text('Run /$picked?'),
            content: Text('Sends the $picked command to the assistant.'),
            actions: [
              TextButton(
                onPressed: () => Navigator.of(ctx).pop(false),
                child: const Text('Cancel'),
              ),
              FilledButton(
                onPressed: () => Navigator.of(ctx).pop(true),
                child: const Text('Run'),
              ),
            ],
          ),
        );
        if (confirmed ?? false) onTrigger(picked);
      },
    );
  }
}

class _CommandSheet extends StatefulWidget {
  const _CommandSheet({required this.session, required this.conversationId});

  final AgentSession session;
  final String conversationId;

  @override
  State<_CommandSheet> createState() => _CommandSheetState();
}

class _CommandSheetState extends State<_CommandSheet> {
  List<Command>? _commands;
  String? _error;

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  Future<void> _refresh() async {
    setState(() {
      _commands = null;
      _error = null;
    });
    try {
      final callId = await widget.session.dispatchTool(
        'Skills',
        '{"detail":true}',
        conversationId: widget.conversationId,
      );
      final frames = <ToolResultFrame>[];
      await for (final frame in widget.session
          .awaitToolResult(callId, conversationId: widget.conversationId)) {
        frames.add(frame);
        if (frame.hasComplete()) break;
      }
      final resp = assembleToolFrames(frames);
      final text = joinTextParts(resp.content);
      if (resp.isError) throw Exception(text);
      final commands = parseCommands(text);
      if (!mounted) return;
      setState(() => _commands = commands);
    } catch (e) {
      if (!mounted) return;
      setState(() => _error = e.toString());
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(
          maxHeight: MediaQuery.of(context).size.height * 0.5,
        ),
        child: _body(scheme),
      ),
    );
  }

  Widget _body(ColorScheme scheme) {
    if (_error != null) {
      return Padding(
        padding: const EdgeInsets.all(24),
        child: Center(
          child: TextButton.icon(
            icon: const Icon(Icons.refresh),
            label: Text(
              'Commands unavailable — retry',
              style: TextStyle(color: scheme.error),
            ),
            onPressed: _refresh,
          ),
        ),
      );
    }
    final commands = _commands;
    if (commands == null) {
      // Static text, not a spinner — matches the rest of the client's
      // "loading" affordances; the gRPC deadline bounds this branch.
      return const Padding(
        padding: EdgeInsets.all(24),
        child: Center(child: Text('Loading commands…')),
      );
    }
    if (commands.isEmpty) {
      return Padding(
        padding: const EdgeInsets.all(24),
        child: Center(
          child: Text(
            'No commands available.',
            style: TextStyle(color: scheme.onSurfaceVariant),
          ),
        ),
      );
    }
    return ListView.builder(
      shrinkWrap: true,
      itemCount: commands.length,
      itemBuilder: (context, i) {
        final c = commands[i];
        return ListTile(
          leading: const Icon(Icons.bolt),
          title: Text(c.name),
          subtitle:
              c.description.isEmpty ? null : _Description(c.description),
          onTap: () => Navigator.of(context).pop(c.name),
        );
      },
    );
  }
}

/// Menu subtitle: a skill's description with inline `code` rendered
/// monospace, clamped to two lines so a description that runs to a full
/// paragraph can't push the rest of the menu off screen — the full skill
/// text is what runs when the command is tapped.
class _Description extends StatelessWidget {
  const _Description(this.description);

  final String description;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final base = theme.textTheme.bodyMedium
        ?.copyWith(color: theme.colorScheme.onSurfaceVariant);
    final code = (base ?? const TextStyle()).copyWith(
      fontFamily: 'monospace',
      background: Paint()..color = theme.colorScheme.surfaceContainer,
    );
    return Text.rich(
      TextSpan(
        children: [
          for (final run in parseDescriptionSpans(description))
            TextSpan(text: run.text, style: run.code ? code : base),
        ],
      ),
      maxLines: 2,
      overflow: TextOverflow.ellipsis,
    );
  }
}
