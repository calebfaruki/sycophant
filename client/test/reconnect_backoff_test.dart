import 'package:flutter_test/flutter_test.dart';
import 'package:sycophant_client/main.dart' show nextReconnectDelay;

/// Backoff math regression. The function mirrors
/// `crates/shared/src/watcher_retry.rs:23-44`: start at `initial`,
/// double per failure, cap at `cap`. The receive-stream reconnect
/// loop in `_ChatScreenState` relies on this curve to prevent the
/// CPU-spinning failure mode where a zero-delay reopen burns one
/// core when the server is unreachable.
void main() {
  const initial = Duration(seconds: 1);
  const cap = Duration(seconds: 30);

  test('zero advances to initial', () {
    expect(nextReconnectDelay(Duration.zero, initial, cap), initial);
  });

  test('doubles per call until the cap', () {
    expect(
      nextReconnectDelay(const Duration(seconds: 1), initial, cap),
      const Duration(seconds: 2),
    );
    expect(
      nextReconnectDelay(const Duration(seconds: 2), initial, cap),
      const Duration(seconds: 4),
    );
    expect(
      nextReconnectDelay(const Duration(seconds: 4), initial, cap),
      const Duration(seconds: 8),
    );
    expect(
      nextReconnectDelay(const Duration(seconds: 8), initial, cap),
      const Duration(seconds: 16),
    );
  });

  test('clamps at the cap before overshooting', () {
    // 16s × 2 = 32s, which overshoots; must clamp to 30s.
    expect(
      nextReconnectDelay(const Duration(seconds: 16), initial, cap),
      cap,
    );
  });

  test('is idempotent at the cap', () {
    expect(nextReconnectDelay(cap, initial, cap), cap);
  });

  test('honors a non-standard initial / cap pair', () {
    // Sanity check that the function is parameterised, not
    // hardcoded — if the caller picks 500ms / 5s, the curve respects it.
    const altInitial = Duration(milliseconds: 500);
    const altCap = Duration(seconds: 5);
    expect(nextReconnectDelay(Duration.zero, altInitial, altCap), altInitial);
    expect(
      nextReconnectDelay(altInitial, altInitial, altCap),
      const Duration(seconds: 1),
    );
    expect(
      nextReconnectDelay(const Duration(seconds: 4), altInitial, altCap),
      altCap,
    );
  });
}
