import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sycophant_client/main.dart'
    show AssistantPartsView, ToolCallCard;
import 'package:sycophant_client/src/turn_parts.dart';
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

/// Streamed-item demux: StreamItem frames route into a turn's typed parts by
/// item_id, deltas append incrementally, and text vs tool parts render as
/// distinct widgets. Mirrors reconnect_stream_test.dart's shape — pure model
/// tests plus a focused widget test, no gRPC.
void main() {
  ItemStart textStart() => ItemStart()..text = TextItem();
  ItemStart toolStart(String name) =>
      ItemStart()..toolUse = (ToolUseItem()..name = name);
  ItemDelta textDelta(String s) => ItemDelta()..textDelta = s;
  ItemDelta toolInput(String s) => ItemDelta()..toolInputJson = s;

  group('StreamedParts routing', () {
    test('two items route to distinct parts by item_id', () {
      final parts = StreamedParts();
      // Interleave a text item and a tool item; deltas for each must land
      // on the correct part despite arriving out of order.
      parts.applyStart('text-1', textStart());
      parts.applyStart('tc-1', toolStart('Bash'));
      parts.applyDelta('text-1', textDelta('hello '));
      parts.applyDelta('tc-1', toolInput('{"cmd":'));
      parts.applyDelta('text-1', textDelta('world'));
      parts.applyDelta('tc-1', toolInput('"ls"}'));

      expect(parts.parts.length, 2);
      final text = parts.parts[0];
      final tool = parts.parts[1];
      expect(text, isA<TextPart>());
      expect(tool, isA<ToolPart>());
      expect((text as TextPart).text.toString(), 'hello world');
      expect((tool as ToolPart).name, 'Bash');
      expect(tool.input.toString(), '{"cmd":"ls"}');
    });

    test('deltas append incrementally as received', () {
      final parts = StreamedParts();
      parts.applyStart('t', textStart());
      parts.applyDelta('t', textDelta('a'));
      expect((parts.parts.single as TextPart).text.toString(), 'a');
      parts.applyDelta('t', textDelta('b'));
      expect((parts.parts.single as TextPart).text.toString(), 'ab');
    });

    test('unknown ItemStart kind is ignored without error', () {
      final parts = StreamedParts();
      // An ItemStart with no known kind set (a future item type) must be
      // dropped, not throw — the forward-compat rule.
      final added = parts.applyStart('mystery', ItemStart());
      expect(added, isFalse);
      expect(parts.parts, isEmpty);
      // A following known item still routes normally.
      expect(parts.applyStart('t', textStart()), isTrue);
      parts.applyDelta('t', textDelta('ok'));
      expect((parts.parts.single as TextPart).text.toString(), 'ok');
    });

    test('delta for unknown item id is dropped', () {
      final parts = StreamedParts();
      parts.applyDelta('ghost', textDelta('nope'));
      expect(parts.parts, isEmpty);
    });
  });

  testWidgets('text and tool parts render as distinct widgets',
      (tester) async {
    final parts = StreamedParts();
    parts.applyStart('t', textStart());
    parts.applyDelta('t', textDelta('some prose'));
    parts.applyStart('tc', toolStart('Bash'));
    parts.applyDelta('tc', toolInput('{"cmd":"ls"}'));

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(body: AssistantPartsView(parts: parts)),
      ),
    );

    // The tool call renders as a distinct labeled ToolCallCard, not folded
    // into the prose text block.
    expect(find.byType(ToolCallCard), findsOneWidget);
    expect(find.text('tool: Bash'), findsOneWidget);
    expect(find.textContaining('{"cmd":"ls"}'), findsOneWidget);
    expect(find.textContaining('some prose'), findsOneWidget);
  });
}
