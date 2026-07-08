import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:grpc/grpc.dart';
import 'package:sycophant_client/main.dart' show ReceiveReconnector;
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

/// Integration regression for the receive-stream reconnect wiring.
///
/// `Stream.listen` defaults `cancelOnError: false`, so a gRPC stream that
/// fails fires BOTH `onError` then `onDone`. The reconnector schedules a
/// reconnect from each handler, so without `cancelOnError: true` the
/// backoff curve advances 2× per failure (2→8→30 instead of
/// 1→2→4→8→16→30). These tests assert the listen-wiring causes the
/// delay to advance exactly once per outage, and that the fatal-code
/// short-circuit + ack-reset still hold.
void main() {
  const initial = Duration(seconds: 1);
  const cap = Duration(seconds: 30);

  /// Build a reconnector wired with delay recorders + no-op UI hooks.
  /// `reopen` is a no-op because the test cancels before the timer
  /// fires (fake-async controls all elapsed time).
  ({ReceiveReconnector reconnector, List<Duration> delays, List<String> fatalEvents})
      buildReconnector() {
    final delays = <Duration>[];
    final fatalEvents = <String>[];
    late final ReceiveReconnector reconnector;
    reconnector = ReceiveReconnector(
      initialDelay: initial,
      maxDelay: cap,
      onAck: (_) {},
      onFrame: (_) {},
      onFatalAuth: () => fatalEvents.add('fatal'),
      onTransientError: () {},
      reopen: () {},
      onDelayAdvance: (d) => delays.add(d),
    );
    return (reconnector: reconnector, delays: delays, fatalEvents: fatalEvents);
  }

  test('errors-then-closes advances delay exactly once', () async {
    final controller = StreamController<ChannelOutbound>();
    final h = buildReconnector();
    h.reconnector.attach(controller.stream);

    controller.addError(GrpcError.unavailable());
    await Future<void>.delayed(Duration.zero);
    await controller.close();
    await Future<void>.delayed(Duration.zero);

    expect(h.delays, equals([initial]));
    await h.reconnector.dispose();
  });

  test('clean server close advances delay exactly once', () async {
    final controller = StreamController<ChannelOutbound>();
    final h = buildReconnector();
    h.reconnector.attach(controller.stream);

    await controller.close();
    await Future<void>.delayed(Duration.zero);

    expect(h.delays, equals([initial]));
    await h.reconnector.dispose();
  });

  test('fatal gRPC code does not schedule reconnect', () async {
    final controller = StreamController<ChannelOutbound>();
    final h = buildReconnector();
    h.reconnector.attach(controller.stream);

    controller.addError(GrpcError.unauthenticated());
    await Future<void>.delayed(Duration.zero);

    expect(h.delays, isEmpty);
    expect(h.fatalEvents, equals(['fatal']));
    await h.reconnector.dispose();
    await controller.close();
  });

  test('all three fatal auth codes route to onFatalAuth, transient does not',
      () async {
    // Drives real GrpcErrors through the live stream listener → _onError →
    // isFatalAuthCode (the SAME predicate the send path uses). Mutant: drop
    // any code from the fatal set → that code advances the reconnect delay
    // instead of firing onFatalAuth, and a version-skew/rotated-signature
    // failure silently retries instead of prompting re-enroll.
    for (final code in [
      StatusCode.permissionDenied,
      StatusCode.unauthenticated,
      StatusCode.unimplemented,
    ]) {
      final controller = StreamController<ChannelOutbound>();
      final h = buildReconnector();
      h.reconnector.attach(controller.stream);

      controller.addError(GrpcError.custom(code));
      await Future<void>.delayed(Duration.zero);

      expect(h.fatalEvents, equals(['fatal']), reason: 'code $code is fatal');
      expect(h.delays, isEmpty, reason: 'code $code must not reconnect');
      await h.reconnector.dispose();
      await controller.close();
    }

    // Boundary: a transient code is NOT fatal — it schedules a reconnect.
    final controller = StreamController<ChannelOutbound>();
    final h = buildReconnector();
    h.reconnector.attach(controller.stream);
    controller.addError(GrpcError.unavailable());
    await Future<void>.delayed(Duration.zero);
    expect(h.fatalEvents, isEmpty);
    expect(h.delays, equals([initial]));
    await h.reconnector.dispose();
    await controller.close();
  });

  test('successful ack resets delay before subsequent failure', () async {
    final controller = StreamController<ChannelOutbound>();
    final h = buildReconnector();
    h.reconnector.attach(controller.stream);

    // Two failures push the delay to 2s.
    controller.addError(GrpcError.unavailable());
    await Future<void>.delayed(Duration.zero);

    // Re-attach because cancelOnError: true cancelled the subscription.
    final controller2 = StreamController<ChannelOutbound>();
    h.reconnector.attach(controller2.stream);
    controller2.addError(GrpcError.unavailable());
    await Future<void>.delayed(Duration.zero);
    expect(h.delays, equals([initial, const Duration(seconds: 2)]));

    // A successful ack resets the curve.
    final controller3 = StreamController<ChannelOutbound>();
    h.reconnector.attach(controller3.stream);
    final ack = ChannelOutbound()..ack = (ChannelAck()..channelId = 'ch-1');
    controller3.add(ack);
    await Future<void>.delayed(Duration.zero);

    // Next failure starts fresh at the initial delay.
    controller3.addError(GrpcError.unavailable());
    await Future<void>.delayed(Duration.zero);

    expect(
      h.delays,
      equals([initial, const Duration(seconds: 2), initial]),
    );

    await h.reconnector.dispose();
    await controller.close();
    await controller2.close();
    await controller3.close();
  });
}
