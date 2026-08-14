// Tests for the "/" command trigger's fetch step. Tapping a command sends the
// skill's BODY as the user message, so the fetch must return the file text and
// must not let a tool error through as if it were skill content.

import 'package:flutter_test/flutter_test.dart';

import 'package:sycophant_client/main.dart' show fetchSkillBody;
import 'package:sycophant_client/src/agent_session.dart';
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

void main() {
  test('fetchSkillBody returns the trimmed skill body', () async {
    final fake = _FakeAgentSession('\n# Classify\n\nDecide the doctype.\n');

    final body = await fetchSkillBody(fake, 'classify',
        conversationId: 'conv-1');

    // Mutant: drop the trim → the leading newline survives and the composer
    // sends a body the empty check would have to re-normalize.
    expect(body, '# Classify\n\nDecide the doctype.');
    // Mutant: hardcode the tool name or drop the name from the input → the
    // harness resolves the wrong skill.
    expect(fake.dispatchedTool, 'Skill');
    expect(fake.lastInput, '{"name":"classify"}');
    // Mutant: pass '' instead of the caller's id → the call's frames land
    // outside the active conversation's execution log.
    expect(fake.dispatchedConversationId, 'conv-1');
  });

  test('fetchSkillBody throws when the tool reports an error', () async {
    final fake = _FakeAgentSession('skill not found: missing', isError: true);

    // Mutant: drop the `resp.isError` throw in `callToolText` → the error
    // string is returned as a body and sent to the assistant as a prompt.
    await expectLater(
      fetchSkillBody(fake, 'missing', conversationId: 'conv-1'),
      throwsA(isA<Exception>().having(
        (e) => e.toString(),
        'message',
        contains('skill not found: missing'),
      )),
    );
  });
}

/// Minimal `AgentSession` double: `dispatchTool` records what it was handed and
/// returns a call_id; `awaitToolResult` replays the canned body as a stdout
/// frame then a terminal whose outcome carries [isError], so the real
/// `assembleToolFrames` derives the error bit the way production does.
/// `noSuchMethod` covers everything the fetch never calls.
class _FakeAgentSession implements AgentSession {
  _FakeAgentSession(this.body, {this.isError = false});
  final String body;
  final bool isError;
  String? dispatchedTool;
  String? lastInput;
  String? dispatchedConversationId;

  @override
  Future<String> dispatchTool(String name, String inputJson,
      {String conversationId = ''}) async {
    dispatchedTool = name;
    lastInput = inputJson;
    dispatchedConversationId = conversationId;
    return 'call-skill';
  }

  @override
  Stream<ToolResultFrame> awaitToolResult(String callId,
      {String conversationId = ''}) async* {
    yield ToolResultFrame()..stdout = body;
    yield ToolResultFrame()
      ..complete = (ToolComplete()
        ..outcome = isError
            ? ToolOutcome.TOOL_OUTCOME_FAILED
            : ToolOutcome.TOOL_OUTCOME_DONE);
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}
