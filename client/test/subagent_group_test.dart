// Acceptance tests (client-activity-ribs) — subagent tree.
//
// Subagent frames arrive as StreamItems carrying parent_conversation_id (the
// PARENT conversation). The client groups children under their parent by that
// correlation id and renders each group collapsible/expandable. Mirrors
// stream_item_test.dart: a pure demux model test plus a focused widget test.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sycophant_client/main.dart' show SubagentGroupTile;
import 'package:sycophant_client/src/turn_parts.dart';
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

void main() {
  StreamItem childFrame(String itemId, String parentConvId, String childConvId) =>
      StreamItem()
        ..itemId = itemId
        ..conversationId = childConvId
        ..parentConversationId = parentConvId
        ..start = (ItemStart()..text = TextItem());

  StreamItem topFrame(String itemId, String convId) => StreamItem()
    ..itemId = itemId
    ..conversationId = convId
    ..start = (ItemStart()..text = TextItem());

  StreamItem namedChildFrame(
    String itemId,
    String parentConvId,
    String childConvId,
    String agentName,
  ) =>
      StreamItem()
        ..itemId = itemId
        ..conversationId = childConvId
        ..parentConversationId = parentConvId
        ..agentName = agentName
        ..start = (ItemStart()..text = TextItem());

  group('SubagentGroups routing by parent<->child correlation id', () {
    test('groups children under their parent by parent_conversation_id', () {
      // EARS: "When subagent events are streamed for a turn, the client shall
      // group them under their parent by the parent<->child correlation
      // identifier."
      // Materiality: key the group by conversation_id (the child) instead of
      // parent_conversation_id -> two children of the same parent land in
      // separate groups and never nest under the active turn.
      final groups = SubagentGroups(parentConversationId: 'parent-1');
      groups.apply(childFrame('a', 'parent-1', 'child-x'));
      groups.apply(childFrame('b', 'parent-1', 'child-x'));
      groups.apply(childFrame('c', 'parent-1', 'child-y'));

      // Two distinct child conversations => two groups under the same parent.
      // Each frame is a text ItemStart that appends one TextPart, so the parts
      // count per child == the frames routed to that child's group.
      expect(groups.childConversationIds.toSet(), {'child-x', 'child-y'});
      expect(groups.partsFor('child-x').parts.length, 2);
      expect(groups.partsFor('child-y').parts.length, 1);
    });

    test('a frame whose parent is not the active turn is not grouped', () {
      // Materiality: drop the parent-match guard -> unrelated subagent frames
      // (belonging to a different active turn) get folded into this turn's tree.
      final groups = SubagentGroups(parentConversationId: 'parent-1');
      final grouped = groups.apply(childFrame('a', 'other-parent', 'child-z'));
      expect(grouped, isFalse);
      expect(groups.childConversationIds, isEmpty);
    });

    test('a top-level frame (no parent link) is not treated as a subagent', () {
      // Materiality: treat an empty parent_conversation_id as a match -> every
      // ordinary turn item is mis-nested as a subagent child.
      final groups = SubagentGroups(parentConversationId: 'parent-1');
      final grouped = groups.apply(topFrame('t', 'parent-1'));
      expect(grouped, isFalse);
      expect(groups.childConversationIds, isEmpty);
    });
  });

  group('SubagentGroups.nameFor records the observed agent name', () {
    test('returns the observed non-empty name for a child', () {
      // EARS: "SubagentGroups.nameFor(child) returns the observed non-empty
      // name, else null." — the positive branch.
      // Materiality: ignore item.agentName in apply (never record it) or have
      // nameFor return the childConversationId -> the tile shows the id hash
      // instead of "poet".
      final groups = SubagentGroups(parentConversationId: 'parent-1');
      groups.apply(namedChildFrame('a', 'parent-1', 'child-x', 'poet'));
      expect(groups.nameFor('child-x'), 'poet');
    });

    test('first non-empty name wins for a child', () {
      // A later empty-name frame for the same child must not clobber the
      // recorded name (frames after the turn-start carry an empty agent_name).
      // Materiality: overwrite the stored name on every frame (last-wins) ->
      // the trailing empty frame erases "poet" and nameFor returns null.
      final groups = SubagentGroups(parentConversationId: 'parent-1');
      groups.apply(namedChildFrame('a', 'parent-1', 'child-x', 'poet'));
      groups.apply(namedChildFrame('b', 'parent-1', 'child-x', ''));
      expect(groups.nameFor('child-x'), 'poet');
    });

    test('returns null when no non-empty name was observed', () {
      // EARS negative branch: a child seen only via empty-name frames has no
      // observed name.
      // Materiality: treat the empty string as an observed name (store it) ->
      // nameFor returns "" instead of null, and the tile renders a blank label
      // instead of falling back to the id-prefix.
      final groups = SubagentGroups(parentConversationId: 'parent-1');
      groups.apply(namedChildFrame('a', 'parent-1', 'child-x', ''));
      expect(groups.nameFor('child-x'), isNull);
    });

    test('returns null for a child that was never grouped', () {
      // Materiality: return a non-null default for an unknown child -> a name
      // is invented for a child that never streamed.
      final groups = SubagentGroups(parentConversationId: 'parent-1');
      expect(groups.nameFor('never-seen'), isNull);
    });
  });

  testWidgets('a subagent group renders collapsed and expands on tap',
      (tester) async {
    // EARS: "Where a subagent group is rendered, the client shall allow it to
    // be collapsed and expanded."
    // Materiality: render the child items in a plain always-visible Column
    // instead of a collapsible tile -> the child content shows before any tap
    // and the expand affordance disappears, failing this test.
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SubagentGroupTile(
            childConversationId: 'child-x',
            children: const [Text('subagent step one')],
          ),
        ),
      ),
    );

    // Collapsed by default: the child content is not shown yet.
    expect(find.text('subagent step one'), findsNothing);

    // Tapping the group header expands it to reveal the child content.
    await tester.tap(find.byType(SubagentGroupTile));
    await tester.pumpAndSettle();
    expect(find.text('subagent step one'), findsOneWidget);
  });

  testWidgets('a named subagent group renders the name, not the id-prefix',
      (tester) async {
    // EARS: "SubagentGroupTile with a non-empty name renders the name, not the
    // id-prefix."
    // Materiality: always render the id-prefix (ignore the name arg) -> the
    // header reads "Sub-agent deadbeef" and the "poet" expectation fails.
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SubagentGroupTile(
            childConversationId: 'deadbeef-cafe-0001',
            name: 'poet',
            children: const [Text('verse')],
          ),
        ),
      ),
    );

    // The operator-authored name is shown; the id hash prefix is not.
    expect(find.textContaining('poet'), findsOneWidget);
    expect(find.textContaining('deadbeef'), findsNothing);
  });

  testWidgets('an unnamed subagent group falls back to the id-prefix',
      (tester) async {
    // The negative side of the same criterion: with no name the tile keeps its
    // existing id-prefix label (name is optional; empty/null => id-prefix).
    // Materiality: unconditionally render the name arg (even when absent) ->
    // the header loses its id-prefix fallback and this fails.
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SubagentGroupTile(
            childConversationId: 'deadbeef-cafe-0001',
            children: const [Text('verse')],
          ),
        ),
      ),
    );

    expect(find.textContaining('deadbeef'), findsOneWidget);
  });
}
