// Tests for the single-writer turn-state reconciler and the gated poller.
// The reconciler is where the subtle rules live: push is authoritative,
// poll is downgrade-only and acts only from `working` (so a stale or late
// poll can never spuriously show a spinner or clobber a fresh send).

import 'package:flutter_test/flutter_test.dart';
import 'package:sycophant_client/main.dart'
    show TurnPhase, TurnStatePoller, TurnStateReconciler;

void main() {
  group('TurnStateReconciler.applyPush', () {
    test('is authoritative — applies every transition', () {
      final r = TurnStateReconciler();
      expect(r.phaseFor('c'), TurnPhase.idle); // default
      r.applyPush('c', TurnPhase.working);
      expect(r.phaseFor('c'), TurnPhase.working);
      r.applyPush('c', TurnPhase.idle);
      expect(r.phaseFor('c'), TurnPhase.idle);
    });

    test('failed carries the reason; reason is hidden when not failed', () {
      final r = TurnStateReconciler();
      r.applyPush('c', TurnPhase.failed, reason: 'worker died');
      expect(r.phaseFor('c'), TurnPhase.failed);
      expect(r.reasonFor('c'), 'worker died');
      // Leaving the failed phase drops the reason.
      r.applyPush('c', TurnPhase.working);
      expect(r.reasonFor('c'), isNull);
    });

    test('failed with empty reason falls back to a default', () {
      final r = TurnStateReconciler();
      r.applyPush('c', TurnPhase.failed);
      expect(r.reasonFor('c'), 'The turn failed.');
    });

    test('null convId maps to the pre-mint sentinel', () {
      final r = TurnStateReconciler();
      r.applyPush(null, TurnPhase.sending);
      expect(r.phaseFor(null), TurnPhase.sending);
      expect(r.phaseFor(TurnStateReconciler.preMintKey), TurnPhase.sending);
    });
  });

  group('TurnStateReconciler.applyPoll (downgrade-only, from working)', () {
    test('settles working → idle', () {
      final r = TurnStateReconciler()..applyPush('c', TurnPhase.working);
      r.applyPoll('c', TurnPhase.idle);
      expect(r.phaseFor('c'), TurnPhase.idle);
    });

    test('settles working → failed with reason', () {
      final r = TurnStateReconciler()..applyPush('c', TurnPhase.working);
      r.applyPoll('c', TurnPhase.failed, reason: 'idle timeout');
      expect(r.phaseFor('c'), TurnPhase.failed);
      expect(r.reasonFor('c'), 'idle timeout');
    });

    test('a working poll never re-asserts working (no-op)', () {
      final r = TurnStateReconciler()..applyPush('c', TurnPhase.working);
      r.applyPoll('c', TurnPhase.working);
      expect(r.phaseFor('c'), TurnPhase.working);
    });

    test('never upgrades idle → working (ignored, not working)', () {
      // The poll exists to catch a missed terminal, not to start a turn.
      // Mutant: drop the `!= working` guard → a stale poll spuriously shows
      // a spinner.
      final r = TurnStateReconciler()..applyPush('c', TurnPhase.idle);
      r.applyPoll('c', TurnPhase.working);
      expect(r.phaseFor('c'), TurnPhase.idle);
    });

    test('a late poll cannot clobber a fresh send (sending is not working)',
        () {
      // working → (poll in flight) → turn ended, user sent again → sending.
      // The late poll(idle) must NOT knock the new send back to idle.
      final r = TurnStateReconciler()..applyPush('c', TurnPhase.sending);
      r.applyPoll('c', TurnPhase.idle);
      expect(r.phaseFor('c'), TurnPhase.sending);
    });
  });

  group('TurnStateReconciler bookkeeping', () {
    test('carry moves phase + reason from pre-mint to the stamped id', () {
      final r = TurnStateReconciler()
        ..applyPush(TurnStateReconciler.preMintKey, TurnPhase.sending);
      r.carry(TurnStateReconciler.preMintKey, 'real-id');
      expect(r.phaseFor('real-id'), TurnPhase.sending);
      expect(r.phaseFor(TurnStateReconciler.preMintKey), TurnPhase.idle);
    });

    test('forget drops a conversation back to default idle', () {
      final r = TurnStateReconciler()..applyPush('c', TurnPhase.working);
      r.forget('c');
      expect(r.phaseFor('c'), TurnPhase.idle);
    });

    test('clearAll empties every conversation', () {
      final r = TurnStateReconciler()
        ..applyPush('a', TurnPhase.working)
        ..applyPush('b', TurnPhase.failed, reason: 'x');
      r.clearAll();
      expect(r.phaseFor('a'), TurnPhase.idle);
      expect(r.phaseFor('b'), TurnPhase.idle);
      expect(r.reasonFor('b'), isNull);
    });
  });

  group('TurnStatePoller.tick', () {
    test('polls when the gate is open, skips when closed', () {
      // Mutant: drop or invert the `shouldPoll()` guard → poll runs while
      // idle (or never runs while working).
      var calls = 0;
      var gate = true;
      final p = TurnStatePoller(
        interval: const Duration(seconds: 7),
        shouldPoll: () => gate,
        poll: () async => calls++,
      );
      p.tick();
      expect(calls, 1);
      gate = false;
      p.tick();
      expect(calls, 1);
      gate = true;
      p.tick();
      expect(calls, 2);
    });
  });
}
