// Tests for the composer's "/" command menu. Two concerns:
//   1. `parseCommands` drops underscore-prefixed (agent-internal) skills.
//   2. The button opens a sheet, asks `Skills` for *detail*, renders each
//      command's name + description, and a tap fires `onTrigger`.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:sycophant_client/src/agent_session.dart';
import 'package:sycophant_client/src/command_menu.dart';
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

void main() {
  test('parseCommands drops underscore-prefixed names', () {
    // Mutant: remove the `!name.startsWith('_')` filter → `_Internal`
    // survives and length is 3.
    final cmds = parseCommands(
      '[{"name":"Classify","description":"Decide doctype."},'
      '{"name":"_Internal","description":"hidden"},'
      '{"name":"Survey","description":"Walk."}]',
    );
    expect(cmds.map((c) => c.name), ['Classify', 'Survey']);
    expect(cmds.first.description, 'Decide doctype.');
  });

  test('parseCommands tolerates a missing description', () {
    // Mutant: a non-null `m['description'] as String` cast throws here.
    final cmds = parseCommands('[{"name":"X"}]');
    expect(cmds.single.description, '');
  });

  test('parseDescriptionSpans flags backtick runs as code', () {
    // Mutant: flip `i.isOdd` → `git mv` loses its code flag (renders as
    // prose, not monospace). Mutant: split on the wrong char → a single
    // plain run, no code segment at all.
    final runs = parseDescriptionSpans('Move with `git mv`, then commit.');
    expect(runs, [
      (text: 'Move with ', code: false),
      (text: 'git mv', code: true),
      (text: ', then commit.', code: false),
    ]);
  });

  test('parseDescriptionSpans yields one plain run with no backticks', () {
    final runs = parseDescriptionSpans('Decide doctype.');
    expect(runs, [(text: 'Decide doctype.', code: false)]);
  });

  test('parseDescriptionSpans drops empty runs from adjacent backticks', () {
    // Leading/adjacent backticks produce empty splits; the `isEmpty`
    // skip keeps them out (mutant: drop the skip → empty code runs leak).
    final runs = parseDescriptionSpans('`x``y`');
    expect(runs, [(text: 'x', code: true), (text: 'y', code: true)]);
  });

  testWidgets('tapping a command sends detail:true and fires onTrigger',
      (tester) async {
    final fake = _FakeAgentSession(
      '[{"name":"Classify","description":"Decide doctype."}]',
    );
    String? triggered;

    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: CommandMenuButton(
          session: fake,
          onTrigger: (name) => triggered = name,
        ),
      ),
    ));

    // Open the sheet.
    await tester.tap(find.byType(CommandMenuButton));
    await tester.pumpAndSettle();

    // Client must request descriptions, not the bare names list.
    expect(fake.lastInput, '{"detail":true}');
    expect(find.text('Classify'), findsOneWidget);
    // The subtitle is now a Text.rich (inline `code` runs render
    // monospace), so match against the flattened rich text.
    expect(find.text('Decide doctype.', findRichText: true), findsOneWidget);

    // Tapping a command now opens a confirm dialog instead of firing
    // immediately — onTrigger must not have run yet.
    await tester.tap(find.text('Classify'));
    await tester.pumpAndSettle();
    expect(triggered, isNull);
    expect(find.text('Run /Classify?'), findsOneWidget);

    // Mutant: drop the dialog→onTrigger wiring → `triggered` stays null
    // even after confirming.
    await tester.tap(find.text('Run'));
    await tester.pumpAndSettle();
    expect(triggered, 'Classify');
  });

  testWidgets('cancelling the confirm dialog leaves onTrigger unfired',
      (tester) async {
    final fake = _FakeAgentSession(
      '[{"name":"Classify","description":"Decide doctype."}]',
    );
    String? triggered;

    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: CommandMenuButton(
          session: fake,
          onTrigger: (name) => triggered = name,
        ),
      ),
    ));

    await tester.tap(find.byType(CommandMenuButton));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Classify'));
    await tester.pumpAndSettle();

    // Mutant: fire onTrigger regardless of the dialog result → cancelling
    // still sets `triggered`.
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(triggered, isNull);
  });
}

/// Minimal `AgentSession` double: `callTool` returns a canned JSON body
/// and records the input it was handed. `noSuchMethod` covers everything
/// the menu never calls.
class _FakeAgentSession implements AgentSession {
  _FakeAgentSession(this.responseJson);
  final String responseJson;
  String? lastInput;

  @override
  Future<CallToolResponse> callTool(String name, String inputJson) async {
    lastInput = inputJson;
    return CallToolResponse()
      ..content.add(ContentBlock()..text = (TextBlock()..text = responseJson));
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}
