// Smoke test: app boots into the enroll screen on a fresh install (no
// persisted credentials). The chat screen + enrollment RPC are exercised
// by the Layer 3 e2e in docs/e2e-test.md, not here — there's no in-process
// gRPC stub in this test.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:sycophant_client/main.dart';

void main() {
  setUp(() {
    SharedPreferences.setMockInitialValues({});
  });

  testWidgets('app shows the enroll screen on first launch', (tester) async {
    await tester.pumpWidget(const SycophantApp());
    // RootScreen kicks off a Future to load preferences; pump until done.
    await tester.pumpAndSettle();

    expect(find.text('Enroll device'), findsOneWidget);
    expect(find.byType(FilledButton), findsOneWidget);
  });
}
