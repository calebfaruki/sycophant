// Acceptance tests (client-activity-ribs) — identity + prompt-change warning.
//
// Turn-start frames carry the agent's identity (name + system_prompt_sha256).
// The client surfaces a per-conversation prompt-change warning when the hash
// differs from the prior turn's in the same conversation. This is a new
// stateful unit in the mould of TurnStateReconciler (per-conversation, testable
// without pumping the tree).

import 'package:flutter_test/flutter_test.dart';
import 'package:sycophant_client/main.dart' show PromptChangeTracker;

void main() {
  group('PromptChangeTracker', () {
    test('first turn in a conversation raises no warning', () {
      // No prior hash to diff against -> nothing to warn about.
      // Materiality: warn on the first observation (no prior) -> a spurious
      // prompt-change banner on every conversation's very first turn.
      final t = PromptChangeTracker();
      expect(t.observe('conv-a', 'sha-1'), isFalse);
    });

    test('same hash on the next turn raises no warning', () {
      // Materiality: warn whenever observe is called (ignore equality) -> the
      // banner fires on every turn even when the prompt never changed.
      final t = PromptChangeTracker()..observe('conv-a', 'sha-1');
      expect(t.observe('conv-a', 'sha-1'), isFalse);
    });

    test('a changed hash in the same conversation raises the warning', () {
      // EARS: "Where a turn's system_prompt_sha256 differs from the prior
      // turn's in the same conversation, the client shall surface a
      // prompt-change warning."
      // Materiality: drop the `!=` compare (or compare against the wrong
      // conversation's stored hash) -> a silently-swapped system prompt goes
      // unflagged.
      final t = PromptChangeTracker()..observe('conv-a', 'sha-1');
      expect(t.observe('conv-a', 'sha-2'), isTrue);
    });

    test('the diff is per-conversation, not global', () {
      // conv-b's first hash must not diff against conv-a's stored hash.
      // Materiality: key the last-seen hash globally instead of per
      // conversation -> switching conversations spuriously warns.
      final t = PromptChangeTracker()..observe('conv-a', 'sha-1');
      expect(t.observe('conv-b', 'sha-9'), isFalse);
    });
  });
}
