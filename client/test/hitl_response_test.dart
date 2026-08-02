// Acceptance tests (client-activity-ribs) — HITL round-trip.
//
// The agent calls a device-renderable tool (RequestUserInput / RequestUserAuth)
// and the harness awaits the client's answer, correlated by request_id.
// These tests pin the two client-side behaviors that are unit-testable without
// a live gRPC channel:
//   1. what the client advertises in supported_methods (capability negotiation),
//   2. the result payload the client returns as the tool call's result.
// The card rendering + request_id echo are exercised by the widget layer /
// Layer-3 e2e; the JSON result shape is where a misread of the protocol bites.

import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:sycophant_client/main.dart'
    show deviceRenderableMethods, hitlInputResult, hitlAuthResult;

void main() {
  group('capability advertisement (supported_methods)', () {
    test('advertises both device-renderable HITL tools plus RevealPath', () {
      // EARS: "While connected, the client shall advertise the device-
      // renderable tools it supports in supported_methods."
      // Materiality: drop 'RequestUserInput' or 'RequestUserAuth' from the
      // advertised set -> the gateway rejects that method server-side before
      // it ever reaches the client (capability negotiation, not refusal), so
      // the tool can never be rendered.
      expect(deviceRenderableMethods, contains('RequestUserInput'));
      expect(deviceRenderableMethods, contains('RequestUserAuth'));
      // RevealPath is the existing fire-and-forget template and stays advertised.
      expect(deviceRenderableMethods, contains('RevealPath'));
    });
  });

  group('RequestUserInput result', () {
    test('returns the chosen action_id as the call result', () {
      // EARS: "When the client receives a RequestUserInput tool call, it shall
      // render its prompt and actions[] and shall return the chosen action_id
      // (and arguments, if any) as that call's result."
      // Materiality: return the prompt / a different key instead of action_id
      // -> the harness's awaiting tool call resolves with the wrong answer.
      final json = hitlInputResult('approve', null);
      final decoded = jsonDecode(json) as Map<String, dynamic>;
      expect(decoded['action_id'], 'approve');
    });

    test('carries arguments alongside the action_id when present', () {
      // Materiality: drop the arguments branch -> a choice that needs a payload
      // (e.g. a typed reason) loses it and the tool resolves incomplete.
      final json = hitlInputResult('submit', {'reason': 'looks good'});
      final decoded = jsonDecode(json) as Map<String, dynamic>;
      expect(decoded['action_id'], 'submit');
      expect(decoded['arguments'], {'reason': 'looks good'});
    });
  });

  group('RequestUserAuth result', () {
    test('completes with a non-error result when the callback resolves', () {
      // EARS: "When the client receives a RequestUserAuth tool call, it shall
      // render the authorization URL and shall complete the call when the
      // external callback resolves."
      // Materiality: return a failure/error result on completion -> the auth
      // tool call is treated as failed even though the user completed it.
      final json = hitlAuthResult();
      final decoded = jsonDecode(json) as Map<String, dynamic>;
      // A resolved auth reports success, not an error payload.
      expect(decoded['error'], isNull);
      expect(decoded['ok'], isTrue);
    });
  });
}
