import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';

import 'agent_session.dart';

/// One button per *user-facing* skill the workspace exposes. Skills are
/// discovered by invoking the `Skills` tool; we drop any whose name
/// starts with `_` (convention: underscore = internal reference,
/// agent-loaded only). Tapping a button sends the literal skill name
/// as a user message — same pattern as ChatGPT's conversation
/// starters. The persona's intent/phase routing turns the trigger
/// into a phase.
class SkillsRow extends StatefulWidget {
  const SkillsRow({
    super.key,
    required this.session,
    required this.onTrigger,
  });

  final AgentSession session;

  /// Callback fired when the user taps a skill. The string is the
  /// skill name; the parent should send it as a user message via the
  /// normal ChannelIngest path.
  final void Function(String skillName) onTrigger;

  @override
  State<SkillsRow> createState() => _SkillsRowState();
}

class _SkillsRowState extends State<SkillsRow> {
  List<String> _skills = const [];
  String? _error;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  Future<void> _refresh() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final resp = await widget.session.callTool('Skills', '{}');
      if (resp.isError) {
        throw Exception(resp.output);
      }
      // Skills returns a JSON array of basenames. Drop underscore-
      // prefixed names — those are agent-internal references and
      // should not appear as user-facing buttons.
      final names = (jsonDecode(resp.output) as List)
          .cast<String>()
          .where((n) => !n.startsWith('_'))
          .toList();
      if (!mounted) return;
      setState(() {
        _skills = names;
        _loading = false;
      });
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _error = e.toString();
        _loading = false;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      // Static text instead of an indeterminate spinner: the spinner's
      // Ticker repaints every frame, which compounds CPU when the
      // `Skills` RPC hangs (server unreachable). `_refresh` flips
      // `_loading=false` on both success and catch paths, so this
      // branch is bounded by the gRPC deadline.
      return const SizedBox(
        height: 48,
        child: Center(child: Text('Loading skills…')),
      );
    }
    if (_error != null) {
      return SizedBox(
        height: 48,
        child: Center(
          child: TextButton.icon(
            icon: const Icon(Icons.refresh, size: 16),
            label: Text(
              'Skills unavailable — retry',
              style: TextStyle(color: Theme.of(context).colorScheme.error),
            ),
            onPressed: _refresh,
          ),
        ),
      );
    }
    if (_skills.isEmpty) return const SizedBox.shrink();
    return SizedBox(
      height: 48,
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
        itemCount: _skills.length,
        separatorBuilder: (_, __) => const SizedBox(width: 6),
        itemBuilder: (context, i) {
          final name = _skills[i];
          return OutlinedButton.icon(
            icon: const Icon(Icons.bolt, size: 16),
            label: Text(name),
            onPressed: () => widget.onTrigger(name),
          );
        },
      ),
    );
  }
}
