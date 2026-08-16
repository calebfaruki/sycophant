// Tests for the cluster→UI turn-phase mapping and the PendingIndicator's
// rendering of each phase — in particular the FAILED affordance (reason
// text, no spinner) that P3B's controller now pushes on teardown.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:sycophant_client/main.dart'
    show PendingIndicator, TurnPhase, turnPhaseFromState;
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

void main() {
  group('turnPhaseFromState', () {
    test('maps WORKING/IDLE/FAILED to their phases', () {
      // Mutant: drop any arm → the matching push stops rendering. FAILED in
      // particular is the P3B bridge; losing it reverts to "Working…" forever.
      expect(turnPhaseFromState(TurnState.TURN_STATE_WORKING), TurnPhase.working);
      expect(turnPhaseFromState(TurnState.TURN_STATE_IDLE), TurnPhase.idle);
      expect(turnPhaseFromState(TurnState.TURN_STATE_FAILED), TurnPhase.failed);
    });

    test('returns null for states the indicator does not render', () {
      // UNSPECIFIED and the reserved THINKING/STOPPING slots must be
      // ignored, not coerced into a phase. Mutant: a catch-all `else
      // return working` would make this non-null.
      expect(turnPhaseFromState(TurnState.TURN_STATE_UNSPECIFIED), isNull);
    });
  });

  group('PendingIndicator', () {
    Widget host(TurnPhase phase, {String? reason}) => MaterialApp(
          home: Scaffold(
            body: PendingIndicator(phase: phase, failureReason: reason),
          ),
        );

    testWidgets('idle renders nothing', (tester) async {
      await tester.pumpWidget(host(TurnPhase.idle));
      expect(find.byType(CircularProgressIndicator), findsNothing);
      expect(find.byIcon(Icons.error_outline), findsNothing);
    });

    testWidgets('sending shows a spinner and Sending…', (tester) async {
      await tester.pumpWidget(host(TurnPhase.sending));
      expect(find.byType(CircularProgressIndicator), findsOneWidget);
      expect(find.text('Sending…'), findsOneWidget);
    });

    testWidgets('working shows a spinner and Working…', (tester) async {
      await tester.pumpWidget(host(TurnPhase.working));
      expect(find.byType(CircularProgressIndicator), findsOneWidget);
      expect(find.text('Working…'), findsOneWidget);
    });

    testWidgets('failed shows the reason and an error glyph, no spinner',
        (tester) async {
      // The whole point of FAILED: an actionable error instead of an
      // endless spinner. Mutant: render a spinner on failed, or drop the
      // reason → one of these expectations fails.
      await tester.pumpWidget(
        host(TurnPhase.failed, reason: 'the prompt job stopped responding'),
      );
      expect(find.byType(CircularProgressIndicator), findsNothing);
      expect(find.byIcon(Icons.error_outline), findsOneWidget);
      expect(find.text('the prompt job stopped responding'), findsOneWidget);
    });

    testWidgets('failed without a reason shows a retry fallback',
        (tester) async {
      await tester.pumpWidget(host(TurnPhase.failed));
      expect(find.byType(CircularProgressIndicator), findsNothing);
      expect(find.textContaining('retry'), findsOneWidget);
    });
  });
}
