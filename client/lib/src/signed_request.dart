// Signed-request envelope for the Tightbeam external listener (ADR
// 013 Q5/Q6). The Rust verifier (`shared::client_signature`) reads the
// `x-sig-*` metadata headers on every external RPC, recomputes the
// body hash + signed payload, and rejects anything that doesn't
// match. Both sides MUST agree byte-for-byte on:
//
//   1. The framed HTTP body bytes that get hashed
//   2. The `method ‖ body-hash ‖ nonce ‖ timestamp` LF-delimited payload
//   3. The signature wire format (DER-encoded ECDSA P-256)
//
// Anything that drifts here produces a uniform PermissionDenied and a
// debugging-from-scratch session.

import 'dart:convert';
import 'dart:math';
import 'dart:typed_data';

import 'package:pointycastle/export.dart';
import 'package:uuid/uuid.dart';

/// Computed envelope for one outbound RPC. Headers mirror
/// `shared::client_signature::SIG_*_HEADER`.
class SignedMetadata {
  SignedMetadata({
    required this.method,
    required this.bodyHashHex,
    required this.nonce,
    required this.timestamp,
    required this.signatureB64,
    required this.kid,
    required this.workspace,
  });

  final String method;
  final String bodyHashHex;
  final String nonce;
  final int timestamp;
  final String signatureB64;
  final String kid;
  final String workspace;

  Map<String, String> toMetadata() => {
        'x-sig-method': method,
        'x-sig-body-hash': bodyHashHex,
        'x-sig-nonce': nonce,
        'x-sig-timestamp': timestamp.toString(),
        'x-sig-signature': signatureB64,
        'x-sig-kid': kid,
        'x-sig-workspace': workspace,
      };
}

final ECDomainParameters _p256 = ECDomainParameters('secp256r1');

/// P-256 keypair held by the client. Stage 4g persists raw bytes via
/// `flutter_secure_storage`; a future iteration will bind the private
/// half to the iOS Secure Enclave / Android Hardware Keystore.
class ClientKeyPair {
  ClientKeyPair({required this.privateScalar, required this.publicSec1}) {
    if (privateScalar.length != 32) {
      throw ArgumentError(
        'privateScalar must be 32 bytes; got ${privateScalar.length}',
      );
    }
    if (publicSec1.length != 65 || publicSec1[0] != 0x04) {
      throw ArgumentError(
        'publicSec1 must be 65 bytes starting with 0x04; got ${publicSec1.length} '
        '(prefix ${publicSec1.isNotEmpty ? publicSec1[0] : -1})',
      );
    }
  }

  /// Raw 32-byte ECDSA private scalar `d`.
  final Uint8List privateScalar;

  /// 65-byte SEC1 uncompressed encoding: `0x04 || X (32) || Y (32)`.
  /// Sent over the wire on RedeemEnrollment; persisted locally so the
  /// signer can reconstruct an `ECPublicKey` without recomputing
  /// `d * G` on every call.
  final Uint8List publicSec1;

  /// Generate a fresh P-256 keypair using a CSPRNG seeded from
  /// `Random.secure()` (OS entropy).
  static ClientKeyPair generate() {
    final params = ECKeyGeneratorParameters(_p256);
    final secureRandom = _seededFortuna();
    final keyGen = ECKeyGenerator()
      ..init(ParametersWithRandom(params, secureRandom));
    final pair = keyGen.generateKeyPair();
    final priv = pair.privateKey as ECPrivateKey;
    final pub = pair.publicKey as ECPublicKey;
    return ClientKeyPair(
      privateScalar: _bigIntToBytes32(priv.d!),
      publicSec1: _publicToSec1(pub),
    );
  }

  /// Sign arbitrary bytes with the held private scalar. Returns raw
  /// `r || s` (64 bytes); the caller converts to DER. Uses RFC 6979
  /// deterministic ECDSA so a given (key, message) pair yields a
  /// stable signature — easier to test, no extra entropy demand at
  /// signing time.
  Uint8List signRaw(Uint8List message) {
    final priv = ECPrivateKey(
      _bytes32ToBigInt(privateScalar),
      _p256,
    );
    final signer = Signer('SHA-256/DET-ECDSA')
      ..init(true, PrivateKeyParameter<ECPrivateKey>(priv));
    final sig = signer.generateSignature(message) as ECSignature;
    final out = Uint8List(64);
    out.setRange(0, 32, _bigIntToBytes32(sig.r));
    out.setRange(32, 64, _bigIntToBytes32(sig.s));
    return out;
  }

  /// Verify a raw `r || s` signature against this keypair's public
  /// half. Exposed for tests + assertion of round-trip correctness;
  /// production verification happens server-side.
  bool verifyRaw(Uint8List message, Uint8List rawRs) {
    if (rawRs.length != 64) return false;
    final pub = ECPublicKey(
      _p256.curve.createPoint(
        _bytes32ToBigInt(publicSec1.sublist(1, 33)),
        _bytes32ToBigInt(publicSec1.sublist(33, 65)),
      ),
      _p256,
    );
    final sig = ECSignature(
      _bytes32ToBigInt(rawRs.sublist(0, 32)),
      _bytes32ToBigInt(rawRs.sublist(32, 64)),
    );
    final verifier = Signer('SHA-256/DET-ECDSA')
      ..init(false, PublicKeyParameter<ECPublicKey>(pub));
    return verifier.verifySignature(message, sig);
  }
}

SecureRandom _seededFortuna() {
  final rng = FortunaRandom();
  final seedSource = Random.secure();
  final seed = Uint8List(32);
  for (var i = 0; i < 32; i++) {
    seed[i] = seedSource.nextInt(256);
  }
  rng.seed(KeyParameter(seed));
  return rng;
}

Uint8List _publicToSec1(ECPublicKey pub) {
  final q = pub.Q!;
  final x = q.x!.toBigInteger()!;
  final y = q.y!.toBigInteger()!;
  final out = Uint8List(65);
  out[0] = 0x04;
  out.setRange(1, 33, _bigIntToBytes32(x));
  out.setRange(33, 65, _bigIntToBytes32(y));
  return out;
}

Uint8List _bigIntToBytes32(BigInt n) {
  if (n.isNegative) {
    throw ArgumentError('expected non-negative BigInt');
  }
  var hex = n.toRadixString(16);
  if (hex.length > 64) {
    throw ArgumentError('BigInt does not fit in 32 bytes (P-256 field)');
  }
  hex = hex.padLeft(64, '0');
  final out = Uint8List(32);
  for (var i = 0; i < 32; i++) {
    out[i] = int.parse(hex.substring(i * 2, i * 2 + 2), radix: 16);
  }
  return out;
}

BigInt _bytes32ToBigInt(Uint8List bytes) {
  var result = BigInt.zero;
  for (final b in bytes) {
    result = (result << 8) | BigInt.from(b);
  }
  return result;
}

/// Frame protobuf bytes the way gRPC's HTTP/2 layer does so the
/// client's hash matches what the Rust middleware sees on the wire.
/// Format: 1-byte compression flag (0 = none) + 4-byte big-endian
/// length + payload. Unary requests carry exactly one frame.
Uint8List frameGrpcMessage(Uint8List protobuf) {
  final out = Uint8List(5 + protobuf.length);
  out[0] = 0;
  final len = protobuf.length;
  out[1] = (len >> 24) & 0xff;
  out[2] = (len >> 16) & 0xff;
  out[3] = (len >> 8) & 0xff;
  out[4] = len & 0xff;
  out.setRange(5, 5 + protobuf.length, protobuf);
  return out;
}

/// Lowercase-hex SHA-256, matching
/// `shared::client_signature::body_hash_hex`.
String bodyHashHex(Uint8List bytes) {
  final digest = SHA256Digest();
  final out = digest.process(bytes);
  return _hex(out);
}

String _hex(List<int> bytes) {
  const hexChars = '0123456789abcdef';
  final out = StringBuffer();
  for (final b in bytes) {
    out.write(hexChars[(b >> 4) & 0xf]);
    out.write(hexChars[b & 0xf]);
  }
  return out.toString();
}

/// Bytes the client signs. Must match
/// `shared::client_signature::signed_payload` byte-for-byte.
Uint8List signedPayload(
  String method,
  String bodyHashHex,
  String nonce,
  int timestamp,
) {
  final out = BytesBuilder();
  out.add(utf8.encode(method));
  out.addByte(0x0a);
  out.add(utf8.encode(bodyHashHex));
  out.addByte(0x0a);
  out.add(utf8.encode(nonce));
  out.addByte(0x0a);
  out.add(utf8.encode(timestamp.toString()));
  return out.toBytes();
}

/// Convert a raw `r || s` P-256 ECDSA signature (64 bytes) to ASN.1
/// DER, matching what `p256::ecdsa::Signature::from_der` expects on
/// the Rust verifier side.
Uint8List rawEcdsaToDer(Uint8List rawRs) {
  if (rawRs.length != 64) {
    throw ArgumentError(
      'raw ECDSA P-256 sig must be 64 bytes; got ${rawRs.length}',
    );
  }
  final r = _derInteger(rawRs.sublist(0, 32));
  final s = _derInteger(rawRs.sublist(32, 64));
  final body = <int>[0x02, r.length, ...r, 0x02, s.length, ...s];
  return Uint8List.fromList([0x30, body.length, ...body]);
}

List<int> _derInteger(Uint8List raw) {
  int start = 0;
  while (start < raw.length - 1 && raw[start] == 0x00) {
    start++;
  }
  final stripped = raw.sublist(start);
  if ((stripped[0] & 0x80) != 0) {
    return [0x00, ...stripped];
  }
  return stripped;
}

/// Build the signed-request metadata for one outbound RPC. The caller
/// hands in the protobuf-encoded request bytes (`request.writeToBuffer()`);
/// this function frames them, hashes the framed bytes, and signs the
/// canonical payload. `uuidGen` is overridable for deterministic tests.
SignedMetadata buildSignedMetadata({
  required String method,
  required Uint8List protobufBytes,
  required String workspace,
  required String clientName,
  required ClientKeyPair keyPair,
  Uuid? uuidGen,
  int? nowSecondsOverride,
}) {
  final framed = frameGrpcMessage(protobufBytes);
  final hashHex = bodyHashHex(framed);
  final nonce = (uuidGen ?? const Uuid()).v4();
  final ts =
      nowSecondsOverride ?? (DateTime.now().millisecondsSinceEpoch ~/ 1000);
  final payload = signedPayload(method, hashHex, nonce, ts);
  final raw = keyPair.signRaw(payload);
  final der = rawEcdsaToDer(raw);
  return SignedMetadata(
    method: method,
    bodyHashHex: hashHex,
    nonce: nonce,
    timestamp: ts,
    signatureB64: base64.encode(der),
    kid: clientName,
    workspace: workspace,
  );
}

/// gRPC method paths the client uses on the external listener. Pinned
/// as constants so a typo doesn't silently cause cross-RPC replay
/// rejections.
class TightbeamMethods {
  static const turn = '/tightbeam.v1.TightbeamController/Turn';
  static const subscribe = '/tightbeam.v1.TightbeamController/Subscribe';
  static const mintConversation =
      '/tightbeam.v1.TightbeamController/MintConversation';
  static const listConversations =
      '/tightbeam.v1.TightbeamController/ListConversations';
}
