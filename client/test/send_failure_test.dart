// Tests for the send-path failure classification. An auth rejection on
// ChannelIngest must route to the persistent sign-out prompt, never an
// inline assistant bubble that masquerades a lifecycle error as a reply.

import 'package:flutter_test/flutter_test.dart';
import 'package:grpc/grpc.dart';

import 'package:sycophant_client/main.dart' show SendFailure, sendFailureDisposition;

void main() {
  group('sendFailureDisposition', () {
    test('permissionDenied is a fatal auth failure', () {
      // Mutant: classify permissionDenied as transport → the auth error goes
      // back to rendering as a fake assistant bubble instead of sign-out.
      expect(
        sendFailureDisposition(GrpcError.permissionDenied('signature rejected')),
        SendFailure.fatalAuth,
      );
    });

    test('unauthenticated and unimplemented are fatal auth failures', () {
      // Mutant: drop unauthenticated/unimplemented from the fatal set → a
      // rotated-signature or post-upgrade version-skew send renders as a fake
      // assistant bubble with the composer live, instead of routing to
      // sign-out/re-enroll like the receive path does.
      expect(
        sendFailureDisposition(GrpcError.unauthenticated('token expired')),
        SendFailure.fatalAuth,
      );
      expect(
        sendFailureDisposition(GrpcError.unimplemented('no such method')),
        SendFailure.fatalAuth,
      );
    });

    test('other gRPC codes are transport failures', () {
      // Mutant: treat everything as fatalAuth → a transient unavailable would
      // wrongly kill the session and demand re-enrollment.
      expect(
        sendFailureDisposition(GrpcError.unavailable('server down')),
        SendFailure.transport,
      );
      expect(
        sendFailureDisposition(GrpcError.internal('boom')),
        SendFailure.transport,
      );
    });

    test('non-gRPC errors are transport failures', () {
      expect(sendFailureDisposition(StateError('socket closed')),
          SendFailure.transport);
    });
  });
}
