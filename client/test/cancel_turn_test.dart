// Acceptance tests (client-activity-ribs) — cancel (local stop), client side.
//
// The client stops an in-flight turn by invoking CancelTurn for that turn's
// identifier (the conversation_id), and treats the pushed CANCELLED turn-state
// as a clean terminal (input re-enabled, no error banner). Both are unit-
// testable without a live channel.

import 'package:flutter_test/flutter_test.dart';
import 'package:sycophant_client/main.dart'
    show TurnPhase, turnPhaseFromState, buildCancelTurnRequest;
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

void main() {
  group('CancelTurn request', () {
    test('is keyed by the turn identifier (the conversation_id)', () {
      // EARS: "When the user activates the cancel control during an in-flight
      // turn, the client shall invoke CancelTurn for that turn's identifier."
      // The turn identifier on the wire is the conversation_id.
      // Materiality: build the request with an empty or wrong conversation_id
      // -> the harness cancels nothing (or the wrong turn).
      final req = buildCancelTurnRequest('conv-42');
      expect(req.conversationId, 'conv-42');
    });
  });

  group('CANCELLED turn-state mapping', () {
    test('maps CANCELLED to a terminal, non-failed phase', () {
      // EARS (client half of the terminal event): a pushed turn_cancelled must
      // end the turn cleanly. CANCELLED is terminal like idle/failed but is NOT
      // failed — the input re-enables with no error banner.
      // Materiality: leave CANCELLED unmapped (returns null) -> a cancelled
      // turn spins on "Working..." forever; or map it to failed -> a spurious
      // error banner on a user-initiated stop.
      final phase = turnPhaseFromState(TurnState.TURN_STATE_CANCELLED);
      expect(phase, isNot(TurnPhase.working));
      expect(phase, isNot(TurnPhase.failed));
      expect(phase, isNotNull);
    });
  });
}
