// Smoke test: app boots into the enroll screen on a fresh install (no
// persisted credentials). The chat screen + enrollment RPC are
// exercised by the Layer 3 e2e in docs/e2e-test.md, not here — there's
// no in-process gRPC stub in this test.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:sycophant_client/main.dart';

const _secureStorageChannel = MethodChannel(
  'plugins.it_nomads.com/flutter_secure_storage',
);

void main() {
  setUp(() {
    // flutter_secure_storage reads/writes go through this MethodChannel.
    // In tests, return empty results so StoredCredentials.load() resolves
    // to null and the app falls through to EnrollScreen.
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(_secureStorageChannel, (call) async {
      switch (call.method) {
        case 'read':
          return null;
        case 'readAll':
          return <String, String>{};
        case 'write':
        case 'delete':
        case 'deleteAll':
          return null;
        case 'containsKey':
          return false;
        default:
          return null;
      }
    });
  });

  tearDown(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(_secureStorageChannel, null);
  });

  testWidgets('app shows the enroll screen on first launch', (tester) async {
    await tester.pumpWidget(const SycophantApp());
    await tester.pumpAndSettle();

    expect(find.text('Enroll device'), findsOneWidget);
    expect(find.byType(FilledButton), findsOneWidget);
  });
}
