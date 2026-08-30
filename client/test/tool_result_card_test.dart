// Tool-result rendering: a completed tool call's OUTPUT (the already-scrubbed
// result carried on the `tool`-role history message) must appear on screen,
// paired to the call that produced it. Pure widget/model tests, no gRPC —
// mirrors stream_item_test.dart's shape.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sycophant_client/main.dart'
    show ToolCallCard, transcriptBubblesFromHistory, streamedTurnHasToolCall;
import 'package:sycophant_client/src/turn_parts.dart';
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

import 'support/content_helpers.dart';

void main() {
  Message userMsg(String text) => Message()
    ..role = 'user'
    ..content.add(textPart(text));

  Message assistantWithCall(String id, String name, String inputJson) => Message()
    ..role = 'assistant'
    ..content.add(textPart('running a tool'))
    ..toolCalls.add(ToolCall()
      ..id = id
      ..name = name
      ..inputJson = inputJson);

  Message toolResult(String toolCallId, String output) => Message()
    ..role = 'tool'
    ..toolCallId = toolCallId
    ..content.add(textPart(output));

  HistoryEntry entry(Message m) => HistoryEntry()..message = m;

  Future<void> pumpCard(WidgetTester tester, ToolCallCard card) =>
      tester.pumpWidget(MaterialApp(home: Scaffold(body: card)));

  group('ToolCallCard output block', () {
    testWidgets('renders the output when non-empty', (tester) async {
      await pumpCard(
        tester,
        const ToolCallCard(
          name: 'test-cmd',
          input: '{"cmd":"cat key"}',
          output: '[REDACTED:demo-ssh-key]',
        ),
      );

      expect(find.text('tool: test-cmd'), findsOneWidget);
      expect(find.textContaining('{"cmd":"cat key"}'), findsOneWidget);
      expect(find.textContaining('[REDACTED:demo-ssh-key]'), findsOneWidget);
    });

    testWidgets('renders no output text when output is empty', (tester) async {
      await pumpCard(
        tester,
        const ToolCallCard(name: 'test-cmd', input: '{"cmd":"ls"}'),
      );

      expect(find.text('tool: test-cmd'), findsOneWidget);
      expect(find.textContaining('{"cmd":"ls"}'), findsOneWidget);
      // No output block, and no stray "output:" label.
      expect(find.textContaining('output:'), findsNothing);
    });
  });

  group('history hydration renders tool results', () {
    testWidgets('pairs a tool result to its call and shows the output',
        (tester) async {
      final entries = [
        entry(userMsg('run the demo command')),
        entry(assistantWithCall('call-1', 'test-cmd', '{"cmd":"cat key"}')),
        entry(toolResult('call-1', '[REDACTED:demo-ssh-key]')),
      ];

      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Column(children: transcriptBubblesFromHistory(entries)),
          ),
        ),
      );

      // The tool result renders as a card paired to the call's name and
      // carrying the scrubbed output.
      expect(find.byType(ToolCallCard), findsOneWidget);
      expect(find.text('tool: test-cmd'), findsOneWidget);
      expect(find.textContaining('[REDACTED:demo-ssh-key]'), findsOneWidget);
      // The user and assistant text turns still render.
      expect(find.textContaining('run the demo command'), findsOneWidget);
      expect(find.textContaining('running a tool'), findsOneWidget);
    });

    testWidgets('an unpaired tool result falls back to name "tool"',
        (tester) async {
      final entries = [
        entry(toolResult('orphan', 'bare output')),
      ];

      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Column(children: transcriptBubblesFromHistory(entries)),
          ),
        ),
      );

      expect(find.text('tool: tool'), findsOneWidget);
      expect(find.textContaining('bare output'), findsOneWidget);
    });
  });

  group('refetch gate', () {
    ItemStart toolStart(String name) =>
        ItemStart()..toolUse = (ToolUseItem()..name = name);
    ItemStart textStart() => ItemStart()..text = TextItem();

    test('true when the streamed turn contained a tool call', () {
      final parts = StreamedParts();
      parts.applyStart('t', textStart());
      parts.applyStart('tc', toolStart('test-cmd'));
      expect(streamedTurnHasToolCall(parts), isTrue);
    });

    test('false for a text-only streamed turn', () {
      final parts = StreamedParts();
      parts.applyStart('t', textStart());
      expect(streamedTurnHasToolCall(parts), isFalse);
    });

    test('false for an empty streamed turn', () {
      expect(streamedTurnHasToolCall(StreamedParts()), isFalse);
    });
  });
}
