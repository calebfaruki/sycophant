// Client-driven tool-call interrupt lifecycle in the browser pane: entering
// pending on the server-minted call_id, offering the interrupt while pending,
// issuing CancelTool for that call's id, and settling the three terminals
// (done / canceled / failure) with an error only on failure.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:sycophant_client/src/agent_session.dart';
import 'package:sycophant_client/src/browser_pane.dart';
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

import 'support/content_helpers.dart';

ToolResultFrame _terminal(ToolOutcome outcome) =>
    ToolResultFrame()..complete = (ToolComplete()..outcome = outcome);

/// Fake session whose preview call resolves on a test-controlled stream, so a
/// pending state is observable before the terminal frame arrives. The listing
/// (`Search`) call replays its rows so a file row exists to tap; the preview
/// (`Preview`) call returns `previewFrames.stream`, which the test drives. Every
/// `cancelTool` invocation is recorded so the exact call_id can be asserted.
class _InterruptSession implements AgentSession {
  _InterruptSession(this.listing);

  final CallToolResponse listing;

  /// The server-minted id returned for the preview dispatch. The interrupt must
  /// carry THIS id, so it is distinctive and asserted verbatim.
  static const previewCallId = 'call-preview-9f3a2b7c';

  final _serverReqCtrl = StreamController<ServerRequest>.broadcast();
  final StreamController<ToolResultFrame> previewFrames =
      StreamController<ToolResultFrame>();
  final List<String> canceledCallIds = [];

  @override
  Stream<ServerRequest> get serverRequests => _serverReqCtrl.stream;

  @override
  Future<String> dispatchTool(String name, String inputJson,
      {String conversationId = ''}) async {
    return name == 'Search' ? 'call-search' : previewCallId;
  }

  @override
  Stream<ToolResultFrame> awaitToolResult(String callId,
      {String conversationId = ''}) {
    if (callId == 'call-search') return _listingStream();
    return previewFrames.stream;
  }

  Stream<ToolResultFrame> _listingStream() async* {
    for (final block in listing.content) {
      if (block.hasText()) {
        yield ToolResultFrame()..stdout = block.text.text;
      }
    }
    yield _terminal(ToolOutcome.TOOL_OUTCOME_DONE);
  }

  @override
  Future<bool> cancelTool(String callId) async {
    canceledCallIds.add(callId);
    return true;
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

/// Open the preview overlay and land it in the pending state: the listing is
/// settled, the file is tapped, and the dispatch has resolved to the pending
/// call_id — but no terminal frame has been pushed, so the call is still in
/// flight. `pumpAndSettle` cannot be used past this point: the pending spinner
/// animates forever, so the terminal must be driven with explicit pumps.
Future<void> _openPreviewPending(
  WidgetTester tester,
  _InterruptSession session,
) async {
  await tester.pumpWidget(
    MaterialApp(home: Scaffold(body: BrowserPane(session: session))),
  );
  await tester.pumpAndSettle(); // listing resolves; file row renders.
  await tester.tap(find.text('photo.png'));
  await tester.pump(); // build the overlay route; initState kicks off dispatch.
  await tester.pump(); // dispatch resolves; overlay enters pending, subscribes.
  await tester.pump(); // flush the pending setState.
}

void main() {
  // Receiving the server-minted call_id enters the pending phase. The pending
  // load state is not observable through the overlay spinner alone — that
  // spinner renders whenever no image and no error are present, regardless of
  // phase — so the phase transition is pinned at its single owner. Only the
  // interrupt affordance is phase-gated, and that is a separate criterion.
  //
  // Materiality: `dispatch` setting `_phase` to anything but `pending` (for
  // example treating the call as immediately resolved) reds the equality.
  test('receiving the server-minted call_id enters the pending phase', () {
    final reconciler = ToolCallReconciler();
    reconciler.dispatch(_InterruptSession.previewCallId);
    expect(reconciler.phase, ToolCallPhase.pending);
  });

  // While the call is pending, the browser pane presents the interrupt
  // affordance (the 'Stop' button, gated by `canInterrupt`).
  //
  // Materiality: hardwiring `canInterrupt` to false, or dropping the button's
  // render guard while pending, removes the button and reds the presence check.
  testWidgets('the interrupt affordance is present while a call is pending',
      (tester) async {
    final session = _InterruptSession(answer([textPart('photo.png')]));
    await _openPreviewPending(tester, session);

    expect(find.text('Stop'), findsOneWidget);
  });

  // Triggering the interrupt while pending issues exactly one CancelTool
  // carrying that pending call's server-minted call_id.
  //
  // Materiality: `_interrupt` passing an empty or wrong id records the wrong
  // value; not calling `cancelTool` records nothing. Either reds the equality,
  // which pins both the single call and the exact id.
  testWidgets('triggering the interrupt cancels the pending call by its id',
      (tester) async {
    final session = _InterruptSession(answer([textPart('photo.png')]));
    await _openPreviewPending(tester, session);

    await tester.tap(find.text('Stop'));
    await tester.pump();

    expect(session.canceledCallIds, [_InterruptSession.previewCallId]);
  });

  // A failing terminal clears the pending state and surfaces an error.
  //
  // Materiality: mapping FAILED to a non-error phase leaves `_error` unset and
  // dismisses the overlay instead of showing the error icon, reding the check.
  testWidgets('a failing terminal clears pending and surfaces an error',
      (tester) async {
    final session = _InterruptSession(answer([textPart('photo.png')]));
    await _openPreviewPending(tester, session);

    session.previewFrames.add(ToolResultFrame()..stderr = 'boom');
    await tester.pump();
    session.previewFrames.add(_terminal(ToolOutcome.TOOL_OUTCOME_FAILED));
    await tester.pumpAndSettle();

    expect(find.byIcon(Icons.error_outline), findsOneWidget);
    expect(find.text('Stop'), findsNothing); // pending cleared.
  });

  // A canceled terminal clears the pending state WITHOUT surfacing an error — a
  // user-initiated cancel is terminal but never shown as a failure.
  //
  // Materiality: mapping CANCELED to the failed phase sets `_error`, keeps the
  // overlay open, and renders the error icon, reding the no-error check.
  testWidgets('a canceled terminal clears pending with no error',
      (tester) async {
    final session = _InterruptSession(answer([textPart('photo.png')]));
    await _openPreviewPending(tester, session);

    session.previewFrames.add(_terminal(ToolOutcome.TOOL_OUTCOME_CANCELED));
    await tester.pumpAndSettle();

    expect(find.byIcon(Icons.error_outline), findsNothing);
    expect(find.text('Stop'), findsNothing); // pending cleared.
  });
}
