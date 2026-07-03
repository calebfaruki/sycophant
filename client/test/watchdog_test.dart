// Virtual-time tests for the two P3C watchdogs:
//   - DeadmanWatchdog: backstop for a turn stuck `working`.
//   - ReceiveReconnector idle-watchdog: force-reconnect a half-open stream.
// fakeAsync lets us elapse the (330s / 100s) timeouts deterministically.

import 'dart:async';

import 'package:fake_async/fake_async.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sycophant_client/main.dart'
    show DeadmanWatchdog, ReceiveReconnector;
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

void main() {
  group('DeadmanWatchdog', () {
    const timeout = Duration(seconds: 330);

    test('fires onExpired exactly at the timeout, then clears', () {
      fakeAsync((async) {
        var fired = 0;
        final w = DeadmanWatchdog(timeout: timeout, onExpired: () => fired++);
        w.arm();
        expect(w.isArmed, isTrue);
        async.elapse(const Duration(seconds: 329));
        expect(fired, 0);
        async.elapse(const Duration(seconds: 1));
        expect(fired, 1);
        expect(w.isArmed, isFalse, reason: 'fires once, then disarms itself');
      });
    });

    test('arm is idempotent while armed — does not push the deadline out', () {
      // The deadman measures time since the turn began working; a 7s poll
      // re-confirming `working` calls arm() again and must NOT reset it.
      // Mutant: make arm() always reset the timer → fired stays 0 here.
      fakeAsync((async) {
        var fired = 0;
        final w = DeadmanWatchdog(timeout: timeout, onExpired: () => fired++);
        w.arm();
        async.elapse(const Duration(seconds: 200));
        w.arm();
        async.elapse(const Duration(seconds: 130)); // 330s since the first arm
        expect(fired, 1);
      });
    });

    test('disarm cancels before firing', () {
      fakeAsync((async) {
        var fired = 0;
        final w = DeadmanWatchdog(timeout: timeout, onExpired: () => fired++);
        w.arm();
        async.elapse(const Duration(seconds: 100));
        w.disarm();
        expect(w.isArmed, isFalse);
        async.elapse(const Duration(seconds: 10000));
        expect(fired, 0);
      });
    });
  });

  group('ReceiveReconnector idle-watchdog', () {
    ReceiveReconnector build(List<String> reopens, {Duration? idleTimeout}) {
      return ReceiveReconnector(
        initialDelay: const Duration(seconds: 1),
        maxDelay: const Duration(seconds: 30),
        onAck: (_) {},
        onFrame: (_) {},
        onFatalAuth: () {},
        onTransientError: () {},
        reopen: () => reopens.add('reopen'),
        idleTimeout: idleTimeout,
      );
    }

    test('force-reconnects after silence past the idle timeout', () {
      // Mutant: drop the _armIdle() in attach → silence never reconnects.
      fakeAsync((async) {
        final reopens = <String>[];
        final controller = StreamController<ChannelOutbound>();
        final r = build(reopens, idleTimeout: const Duration(seconds: 100));
        r.attach(controller.stream);
        async.elapse(const Duration(seconds: 99));
        expect(reopens, isEmpty);
        async.elapse(const Duration(seconds: 1));
        expect(reopens, ['reopen']);
        r.dispose();
      });
    });

    test('an inbound frame resets the idle clock', () {
      // Mutant: drop the _armIdle() reset in _onData → the clock would fire
      // 100s after attach regardless of liveness.
      fakeAsync((async) {
        final reopens = <String>[];
        final controller = StreamController<ChannelOutbound>();
        final r = build(reopens, idleTimeout: const Duration(seconds: 100));
        r.attach(controller.stream);
        async.elapse(const Duration(seconds: 80));
        controller.add(ChannelOutbound()..ack = (ChannelAck()..channelId = 'c'));
        async.flushMicrotasks();
        async.elapse(const Duration(seconds: 80)); // only 80s since the frame
        expect(reopens, isEmpty, reason: 'a frame must reset the clock');
        async.elapse(const Duration(seconds: 21)); // now 101s since the frame
        expect(reopens, ['reopen']);
        r.dispose();
      });
    });

    test('no watchdog when idleTimeout is null (existing reconnect tests)', () {
      fakeAsync((async) {
        final reopens = <String>[];
        final controller = StreamController<ChannelOutbound>();
        final r = build(reopens, idleTimeout: null);
        r.attach(controller.stream);
        async.elapse(const Duration(seconds: 10000));
        expect(reopens, isEmpty);
        r.dispose();
      });
    });
  });
}
