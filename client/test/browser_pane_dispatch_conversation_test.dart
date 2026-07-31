// An app-driven tool dispatch carries the active conversation id.
//
// Pins that `_activeConvId` reaches `AgentSession.dispatchTool`
// as its `conversationId` when the browser previews a file — the request must
// carry the active conversation, never empty.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:sycophant_client/src/agent_session.dart';
import 'package:sycophant_client/src/browser_pane.dart';
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

import 'support/content_helpers.dart';

/// Records the `conversationId` the browser passes to `dispatchTool`, keyed by
/// tool name. Both the folder listing (`Search`) and the file preview
/// (`Preview`) now dispatch, so the recorder keeps them apart and the test pins
/// the preview dispatch specifically. `awaitToolResult` replays the listing
/// frames for the `Search` call so a file row exists to tap.
class _RecordingSession implements AgentSession {
  _RecordingSession(this.listing);

  final CallToolResponse listing;
  final Map<String, String> conversationByTool = {};
  final _serverReqCtrl = StreamController<ServerRequest>.broadcast();

  @override
  Stream<ServerRequest> get serverRequests => _serverReqCtrl.stream;

  @override
  Future<String> dispatchTool(String name, String inputJson,
      {String conversationId = ''}) async {
    conversationByTool[name] = conversationId;
    return name == 'Search' ? 'call-search' : 'call-preview';
  }

  @override
  Stream<ToolResultFrame> awaitToolResult(String callId,
      {String conversationId = ''}) async* {
    if (callId == 'call-search') {
      for (final block in listing.content) {
        if (block.hasText()) {
          yield ToolResultFrame()..stdout = block.text.text;
        }
      }
    }
    yield ToolResultFrame()
      ..complete = (ToolComplete()..outcome = ToolOutcome.TOOL_OUTCOME_DONE);
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  // Materiality: the browser must thread the active conversation id all the way
  // to the FILE-PREVIEW `dispatchTool` call. A mutant that passes an empty
  // string (or drops the threading through BrowserPane / _PreviewOverlay)
  // records '' under 'Preview' instead of the active id and reds the equality
  // below. The assertion pins the preview call by name, so it is not satisfied
  // by the listing (`Search`) dispatch and fails non-vacuously.
  testWidgets('a file-preview dispatch carries the active conversation id',
      (tester) async {
    const activeConv = 'active-conv-11111111-2222-3333-4444-555555555555';
    final session = _RecordingSession(answer([textPart('photo.png')]));

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: BrowserPane(session: session, conversationId: activeConv),
        ),
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.text('photo.png'));
    await tester.pumpAndSettle();

    expect(
      session.conversationByTool['Preview'],
      activeConv,
      reason:
          'the browser dispatches the preview tool under the active conversation, not empty',
    );
  });
}
