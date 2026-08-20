// Race-condition regression for `ConversationsDrawer`. The drawer
// kicks `_refresh` from both `initState` and `didUpdateWidget` and
// optimistically mutates `_summaries` on delete. Each test pins one
// invariant the request-id guard must hold against an in-flight
// `listConversations`.

import 'dart:async';

import 'package:fixnum/fixnum.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:sycophant_client/src/agent_session.dart';
import 'package:sycophant_client/src/conversations_drawer.dart';
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

void main() {
  testWidgets('older response cannot overwrite newer snapshot',
      (tester) async {
    final fake = _FakeAgentSession();

    await tester.pumpWidget(_host(session: fake, refreshTick: 0));
    // initState fired refresh #1; capture and arm refresh #2.
    final completerA = fake.takeNextCompleter();

    await tester.pumpWidget(_host(session: fake, refreshTick: 1));
    await tester.pump();
    final completerB = fake.takeNextCompleter();

    completerB.complete(_summaries(['A', 'B', 'C_new']));
    await tester.pumpAndSettle();
    completerA.complete(_summaries(['A', 'B', 'C_old']));
    await tester.pumpAndSettle();

    expect(find.text('C_new'), findsOneWidget);
    expect(find.text('C_old'), findsNothing);
  });

  testWidgets('stale response after optimistic delete cannot resurrect a row',
      (tester) async {
    final fake = _FakeAgentSession();

    await tester.pumpWidget(_host(session: fake, refreshTick: 0));
    final completerA = fake.takeNextCompleter();

    // Resolve the initial load so the list renders and the delete
    // affordance is reachable.
    completerA.complete(_summaries(['keep', 'doomed']));
    await tester.pump();
    expect(find.text('doomed'), findsOneWidget);

    // Bump refreshTick to start refresh #2; capture its completer
    // but leave it pending — this is the "stale" call.
    await tester.pumpWidget(_host(session: fake, refreshTick: 1));
    await tester.pump();
    final completerStale = fake.takeNextCompleter();

    // Tap delete on `doomed`, confirm in the dialog.
    await tester.tap(find.byTooltip('Delete conversation').last);
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, 'Delete'));
    await tester.pumpAndSettle();
    expect(find.text('doomed'), findsNothing);

    // The in-flight refresh resolves with the pre-delete row set.
    completerStale.complete(_summaries(['keep', 'doomed']));
    await tester.pumpAndSettle();

    expect(find.text('doomed'), findsNothing);
    expect(find.text('keep'), findsOneWidget);
  });

  testWidgets('rename rollback restores prior name on RPC failure',
      (tester) async {
    // Mutation target: remove the rollback `setState` inside
    // `_renameConversation`'s catch block — the optimistic "renamed"
    // would survive the failure and the assertion at the end of this
    // test goes red because the original name no longer appears.
    final fake = _FakeAgentSession();

    await tester.pumpWidget(_host(session: fake, refreshTick: 0));
    final initialLoad = fake.takeNextCompleter();
    initialLoad.complete(_summaries(['original']));
    await tester.pumpAndSettle();
    expect(find.text('original'), findsOneWidget);

    // Open rename dialog, type a new name, confirm.
    await tester.tap(find.byTooltip('Rename conversation'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'attempted');
    await tester.tap(find.widgetWithText(FilledButton, 'Rename'));
    await tester.pumpAndSettle();
    // Optimistic update shows the new name immediately.
    expect(find.text('attempted'), findsOneWidget);

    // Server rejects → drawer rolls back.
    fake.takeNextRenameCompleter().completeError(Exception('nope'));
    await tester.pumpAndSettle();
    expect(find.text('attempted'), findsNothing);
    expect(find.text('original'), findsOneWidget);
  });

  testWidgets(
      'stale rename completion cannot clobber a newer rename of the same row',
      (tester) async {
    // Mutation target: remove the `id != _reqId` guard in the catch
    // block of `_renameConversation` — the first (older) rename's
    // delayed failure would roll back to the original name and shadow
    // the second (newer) rename's success.
    final fake = _FakeAgentSession();

    await tester.pumpWidget(_host(session: fake, refreshTick: 0));
    fake.takeNextCompleter().complete(_summaries(['original']));
    await tester.pumpAndSettle();

    // Rename #1 → "first"
    await tester.tap(find.byTooltip('Rename conversation'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'first');
    await tester.tap(find.widgetWithText(FilledButton, 'Rename'));
    await tester.pumpAndSettle();
    final renameFirst = fake.takeNextRenameCompleter();

    // Rename #2 → "second" (UI shows "first" optimistically right now)
    await tester.tap(find.byTooltip('Rename conversation'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'second');
    await tester.tap(find.widgetWithText(FilledButton, 'Rename'));
    await tester.pumpAndSettle();
    final renameSecond = fake.takeNextRenameCompleter();
    expect(find.text('second'), findsOneWidget);

    // Newer rename succeeds first.
    renameSecond.complete();
    await tester.pumpAndSettle();
    // Older rename fails afterwards — its catch must be stamped out.
    renameFirst.completeError(Exception('slow failure'));
    await tester.pumpAndSettle();

    expect(find.text('second'), findsOneWidget);
    expect(find.text('original'), findsNothing);
    expect(find.text('first'), findsNothing);
  });

  testWidgets(
      'retry-button taps while a refresh is in flight do not produce a clobber',
      (tester) async {
    final fake = _FakeAgentSession();

    await tester.pumpWidget(_host(session: fake, refreshTick: 0));
    final completerFirst = fake.takeNextCompleter();
    // Force the error branch so the retry button renders.
    completerFirst.completeError(Exception('boom'));
    await tester.pump();
    expect(find.textContaining('Failed to load'), findsOneWidget);

    // Tap retry — refresh #1 in this scenario.
    await tester.tap(find.textContaining('Failed to load'));
    await tester.pump();
    final completerR1 = fake.takeNextCompleter();

    // Tap retry again before #1 resolves — refresh #2.
    // The error branch is still showing because #1 hasn't completed yet.
    await tester.tap(find.textContaining('Failed to load'));
    await tester.pump();
    final completerR2 = fake.takeNextCompleter();

    completerR2.complete(_summaries(['winner']));
    await tester.pumpAndSettle();
    completerR1.complete(_summaries(['loser']));
    await tester.pumpAndSettle();

    expect(find.text('winner'), findsOneWidget);
    expect(find.text('loser'), findsNothing);
  });
}

Widget _host({required AgentSession session, required int refreshTick}) {
  return MaterialApp(
    home: Scaffold(
      body: ConversationsDrawer(
        session: session,
        activeConvId: null,
        refreshTick: refreshTick,
        onPick: (_) {},
        onNew: () async {},
        onDeleted: (_) {},
      ),
    ),
  );
}

List<ConversationSummary> _summaries(List<String> ids) {
  // Names default to the id so `find.text(id)` assertions still match: the row
  // title is the server-supplied `name` field, not a client-truncated id.
  return ids
      .map((id) => ConversationSummary(
            conversationId: id,
            lastTouchedMsEpoch: Int64(0),
            name: id,
          ))
      .toList(growable: false);
}

/// Test double that hands every `listConversations` call its own
/// `Completer` so the test controls resolution order. `noSuchMethod`
/// covers the parts of `AgentSession` the drawer never touches.
class _FakeAgentSession implements AgentSession {
  final List<Completer<List<ConversationSummary>>> _pending = [];
  final List<Completer<void>> _pendingRenames = [];
  final List<String> renameNames = [];

  Completer<List<ConversationSummary>> takeNextCompleter() {
    expect(_pending, isNotEmpty,
        reason: 'expected a pending listConversations call');
    return _pending.removeAt(0);
  }

  Completer<void> takeNextRenameCompleter() {
    expect(_pendingRenames, isNotEmpty,
        reason: 'expected a pending setConversationName call');
    return _pendingRenames.removeAt(0);
  }

  @override
  Future<List<ConversationSummary>> listConversations() {
    final c = Completer<List<ConversationSummary>>();
    _pending.add(c);
    return c.future;
  }

  @override
  Future<void> deleteConversation(String conversationId) async {}

  @override
  Future<void> setConversationName(String conversationId, String name) {
    renameNames.add(name);
    final c = Completer<void>();
    _pendingRenames.add(c);
    return c.future;
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}
