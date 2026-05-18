// Unit tests for `src/signed_request.dart`. These pin the wire-format
// contract between the Flutter client and the Rust verifier in
// `shared::client_signature` — any drift here produces a uniform
// PermissionDenied on the controller side that's painful to debug.

import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';

import 'package:sycophant_client/src/signed_request.dart';

void main() {
  group('frameGrpcMessage', () {
    test('prefixes 5 bytes: 0x00 flag + big-endian length', () {
      final framed = frameGrpcMessage(Uint8List.fromList([0xde, 0xad, 0xbe, 0xef]));
      expect(framed.length, 9);
      expect(framed[0], 0x00, reason: 'compression flag = none');
      expect(framed[1], 0x00);
      expect(framed[2], 0x00);
      expect(framed[3], 0x00);
      expect(framed[4], 0x04, reason: 'length = 4 (LSB)');
      expect(framed.sublist(5), [0xde, 0xad, 0xbe, 0xef]);
    });

    test('zero-length payload produces a 5-byte frame', () {
      final framed = frameGrpcMessage(Uint8List(0));
      expect(framed.length, 5);
      expect(framed.sublist(1, 5), [0, 0, 0, 0]);
    });

    test('length encodes big-endian for payloads > 255 bytes', () {
      final framed = frameGrpcMessage(Uint8List(0x0102));
      expect(framed[1], 0x00);
      expect(framed[2], 0x00);
      expect(framed[3], 0x01);
      expect(framed[4], 0x02);
    });
  });

  group('bodyHashHex', () {
    test('empty bytes match the known SHA-256 test vector', () {
      // RFC 6234 vector: sha256("") = e3b0c44...b855. Same one the Rust
      // side asserts in shared/src/client_signature.rs.
      expect(
        bodyHashHex(Uint8List(0)),
        'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
      );
    });

    test('returns lowercase hex', () {
      final h = bodyHashHex(Uint8List.fromList([0xff]));
      expect(h, h.toLowerCase());
      expect(h.length, 64);
    });

    test('different inputs produce different hashes', () {
      final a = bodyHashHex(Uint8List.fromList([1, 2, 3]));
      final b = bodyHashHex(Uint8List.fromList([1, 2, 4]));
      expect(a, isNot(equals(b)));
    });
  });

  group('signedPayload', () {
    test('LF-delimited in the order method/hash/nonce/timestamp', () {
      final bytes = signedPayload('/M', 'abc', 'nonce', 123);
      expect(utf8.decode(bytes), '/M\nabc\nnonce\n123');
    });

    test('field-order changes produce different bytes', () {
      final a = signedPayload('/M', 'abc', 'nonce', 123);
      final mixed = signedPayload('/M', 'nonce', 'abc', 123);
      expect(a, isNot(equals(mixed)));
    });

    test('is deterministic across calls with same inputs', () {
      final a = signedPayload('/M', 'abc', 'nonce', 123);
      final b = signedPayload('/M', 'abc', 'nonce', 123);
      expect(a, equals(b));
    });
  });

  group('rawEcdsaToDer', () {
    test('rejects non-64-byte input', () {
      expect(() => rawEcdsaToDer(Uint8List(63)), throwsArgumentError);
      expect(() => rawEcdsaToDer(Uint8List(65)), throwsArgumentError);
    });

    test('produces SEQUENCE { INTEGER, INTEGER } framing', () {
      final raw = Uint8List(64);
      for (var i = 0; i < 32; i++) {
        raw[i] = 0x01;
        raw[32 + i] = 0x02;
      }
      final der = rawEcdsaToDer(raw);
      expect(der[0], 0x30, reason: 'SEQUENCE tag');
      expect(der[1], 68); // 2 + 32 + 2 + 32
      expect(der[2], 0x02);
      expect(der[3], 32);
      expect(der[2 + 2 + 32], 0x02);
    });

    test('prepends 0x00 when MSB is set so DER reads as positive', () {
      final raw = Uint8List(64);
      raw[0] = 0x80;
      for (var i = 1; i < 32; i++) {
        raw[i] = 0x01;
      }
      for (var i = 32; i < 64; i++) {
        raw[i] = 0x01;
      }
      final der = rawEcdsaToDer(raw);
      expect(der[2], 0x02);
      expect(der[3], 33);
      expect(der[4], 0x00);
      expect(der[5], 0x80);
    });

    test('strips leading zero bytes when MSB of next byte is unset', () {
      final raw = Uint8List(64);
      raw[2] = 0x01;
      for (var i = 3; i < 32; i++) {
        raw[i] = 0x02;
      }
      for (var i = 32; i < 64; i++) {
        raw[i] = 0x03;
      }
      final der = rawEcdsaToDer(raw);
      expect(der[2], 0x02);
      expect(der[3], 30);
      expect(der[4], 0x01);
    });
  });

  group('ClientKeyPair', () {
    test('generate produces 32-byte private + 65-byte SEC1 public', () {
      final kp = ClientKeyPair.generate();
      expect(kp.privateScalar.length, 32);
      expect(kp.publicSec1.length, 65);
      expect(kp.publicSec1[0], 0x04, reason: 'SEC1 uncompressed marker');
    });

    test('rejects a private scalar of wrong length', () {
      expect(
        () => ClientKeyPair(
          privateScalar: Uint8List(31),
          publicSec1: Uint8List(65)..[0] = 0x04,
        ),
        throwsArgumentError,
      );
    });

    test('rejects a public key without 0x04 prefix', () {
      expect(
        () => ClientKeyPair(
          privateScalar: Uint8List(32),
          publicSec1: Uint8List(65),
        ),
        throwsArgumentError,
      );
    });

    test('signRaw produces a 64-byte signature that verifies with the public key', () {
      // Round-trip: sign with the private half, verify with the public
      // half via the same pointycastle primitives. Confirms key
      // serialization, signing, and the raw r||s shape.
      final kp = ClientKeyPair.generate();
      final message = Uint8List.fromList(utf8.encode('hello'));
      final sig = kp.signRaw(message);
      expect(sig.length, 64);
      expect(kp.verifyRaw(message, sig), isTrue);
    });

    test('signRaw rejects a tampered message on verify', () {
      final kp = ClientKeyPair.generate();
      final original = Uint8List.fromList(utf8.encode('hello'));
      final tampered = Uint8List.fromList(utf8.encode('hellp'));
      final sig = kp.signRaw(original);
      expect(kp.verifyRaw(tampered, sig), isFalse);
    });

    test('DET-ECDSA: same (key, message) yields the same signature', () {
      // Deterministic ECDSA via RFC 6979 — same key + same message →
      // same r||s. Catches a regression to non-deterministic signing.
      final kp = ClientKeyPair.generate();
      final message = Uint8List.fromList(utf8.encode('hello'));
      final a = kp.signRaw(message);
      final b = kp.signRaw(message);
      expect(a, equals(b));
    });
  });

  group('buildSignedMetadata', () {
    test('populates all 7 x-sig-* headers', () {
      final kp = ClientKeyPair.generate();
      final sig = buildSignedMetadata(
        method: TightbeamMethods.channelIngest,
        protobufBytes: Uint8List.fromList([1, 2, 3]),
        workspace: 'hello-world',
        clientName: 'calebs-iphone',
        keyPair: kp,
      );
      final m = sig.toMetadata();
      expect(m['x-sig-method'], TightbeamMethods.channelIngest);
      expect(m['x-sig-body-hash'], hasLength(64));
      expect(m['x-sig-nonce'], isNotEmpty);
      expect(m['x-sig-timestamp'], matches(RegExp(r'^\d+$')));
      expect(m['x-sig-signature'], isNotEmpty);
      expect(m['x-sig-kid'], 'calebs-iphone');
      expect(m['x-sig-workspace'], 'hello-world');
    });

    test('respects nowSecondsOverride for deterministic test fixtures', () {
      final kp = ClientKeyPair.generate();
      final sig = buildSignedMetadata(
        method: TightbeamMethods.channelIngest,
        protobufBytes: Uint8List(0),
        workspace: 'ws',
        clientName: 'kid',
        keyPair: kp,
        nowSecondsOverride: 1700000000,
      );
      expect(sig.timestamp, 1700000000);
      expect(sig.toMetadata()['x-sig-timestamp'], '1700000000');
    });

    test('different protobuf bytes produce different body hashes', () {
      final kp = ClientKeyPair.generate();
      final a = buildSignedMetadata(
        method: TightbeamMethods.channelIngest,
        protobufBytes: Uint8List.fromList([1]),
        workspace: 'ws',
        clientName: 'kid',
        keyPair: kp,
      );
      final b = buildSignedMetadata(
        method: TightbeamMethods.channelIngest,
        protobufBytes: Uint8List.fromList([2]),
        workspace: 'ws',
        clientName: 'kid',
        keyPair: kp,
      );
      expect(a.bodyHashHex, isNot(equals(b.bodyHashHex)));
    });

    test('produced signature DER-decodes and verifies via raw round-trip', () {
      // Defends the wire contract end-to-end on the client side: build
      // metadata → base64-decode the signature → parse DER back to
      // raw r||s → verify against the public key. If any step drifts
      // the server will reject; this test catches it locally.
      final kp = ClientKeyPair.generate();
      final sig = buildSignedMetadata(
        method: TightbeamMethods.channelIngest,
        protobufBytes: Uint8List.fromList([7, 8, 9]),
        workspace: 'ws',
        clientName: 'kid',
        keyPair: kp,
        nowSecondsOverride: 1700000000,
      );
      final der = base64.decode(sig.signatureB64);
      final raw = _derToRawEcdsa(der);
      final payload = signedPayload(
        sig.method,
        sig.bodyHashHex,
        sig.nonce,
        sig.timestamp,
      );
      expect(kp.verifyRaw(payload, raw), isTrue);
    });
  });

  group('TightbeamMethods constants', () {
    test('channelIngest path matches server-side constant', () {
      expect(
        TightbeamMethods.channelIngest,
        '/tightbeam.v1.TightbeamController/ChannelIngest',
      );
    });
    test('channelReceive path matches server-side constant', () {
      expect(
        TightbeamMethods.channelReceive,
        '/tightbeam.v1.TightbeamController/ChannelReceive',
      );
    });
  });
}

/// Decode a DER-encoded ECDSA P-256 signature back to raw `r || s`.
/// Test-side helper for the round-trip assertion.
Uint8List _derToRawEcdsa(Uint8List der) {
  if (der[0] != 0x30) throw FormatException('expected SEQUENCE');
  var i = 2; // skip tag + length
  if (der[i] != 0x02) throw FormatException('expected INTEGER for r');
  i++;
  final rLen = der[i++];
  var r = der.sublist(i, i + rLen);
  i += rLen;
  if (der[i] != 0x02) throw FormatException('expected INTEGER for s');
  i++;
  final sLen = der[i++];
  var s = der.sublist(i, i + sLen);
  // Strip a leading 0x00 padding byte if present (DER positive-sign).
  if (r.length == 33 && r[0] == 0x00) r = r.sublist(1);
  if (s.length == 33 && s[0] == 0x00) s = s.sublist(1);
  // Left-pad to 32 bytes.
  final out = Uint8List(64);
  out.setRange(32 - r.length, 32, r);
  out.setRange(64 - s.length, 64, s);
  return out;
}
