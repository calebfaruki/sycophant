// Acceptance tests: tool-result media contract (client side).
//
// The workspace browser is the tool-agnostic consumer of a tool answer's
// content parts: it renders a text-only answer as file rows, and it renders an
// image part a file tap returns in a full-screen overlay rather than inline in
// the row.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:sycophant_client/src/agent_session.dart';
import 'package:sycophant_client/src/browser_pane.dart';
import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

import 'support/content_helpers.dart';

/// A valid 1x1 PNG so `Image.memory` decodes without throwing in the harness.
const List<int> _onePixelPng = [
  137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, //
  0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 31, 21, 196, 137, //
  0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 250, 207, 0, 0, //
  0, 3, 1, 1, 0, 24, 221, 141, 219, 0, 0, 0, 0, 73, 69, 78, 68, //
  174, 66, 96, 130,
];

ContentBlock _imagePart(String mediaType, List<int> bytes) =>
    ContentBlock()..image = (ImageBlock()..mediaType = mediaType..data = bytes);

/// Minimal `AgentSession` double. Both the folder listing and the file preview
/// now flow through `dispatchTool` + `awaitToolResult` (there is no unary
/// convenience wrapper anymore). `awaitToolResult` only sees a call_id, so
/// `dispatchTool`
/// hands back a distinct id per tool name ('call-search' vs 'call-preview') and
/// `awaitToolResult` branches on that id to replay the matching answer's parts
/// as frames (image part -> image frame), so the browser renders each exactly
/// as the live split would.
class _FakeSession implements AgentSession {
  _FakeSession({required this.listing, required this.preview});

  final CallToolResponse listing;
  final CallToolResponse preview;
  final _serverReqCtrl = StreamController<ServerRequest>.broadcast();

  @override
  Stream<ServerRequest> get serverRequests => _serverReqCtrl.stream;

  @override
  Future<String> dispatchTool(String name, String inputJson,
          {String conversationId = ''}) async =>
      name == 'Search' ? 'call-search' : 'call-preview';

  @override
  Stream<ToolResultFrame> awaitToolResult(String callId,
      {String conversationId = ''}) async* {
    final answer = callId == 'call-search' ? listing : preview;
    for (final block in answer.content) {
      if (block.hasImage()) {
        yield ToolResultFrame()..image = block.image;
      } else if (block.hasText()) {
        yield ToolResultFrame()..stdout = block.text.text;
      }
    }
    yield ToolResultFrame()
      ..complete = (ToolComplete()..outcome = ToolOutcome.TOOL_OUTCOME_DONE);
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

Future<void> _pumpBrowser(WidgetTester tester, _FakeSession session) async {
  await tester.pumpWidget(
    MaterialApp(home: Scaffold(body: BrowserPane(session: session))),
  );
  await tester.pumpAndSettle();
}

void main() {
  // The listing answer is text-only; the browser must read the text out of the
  // content parts and render the file row. No image is shown.
  //
  // Materiality: the browser must read the joined text of the answer's content
  // parts (not a removed `output` string, not the image parts). A mutant that
  // reads nothing / drops text parts renders no rows and reds `photo.png`.
  testWidgets('a text-only answer renders as file-row text, no image',
      (tester) async {
    final session = _FakeSession(
      listing: answer([textPart('photo.png')]),
      preview: answer([textPart('unused')]),
    );
    await _pumpBrowser(tester, session);

    expect(find.text('photo.png'), findsOneWidget);
    expect(find.byType(Image), findsNothing);
  });

  // Tapping a file whose preview answer carries an image part must render that
  // image.
  //
  // Materiality: the file tap must invoke the preview tool, walk the returned
  // content parts, and render the image part. A mutant that leaves the file
  // `onTap` null (or ignores image parts) renders no `Image` and reds this.
  testWidgets('an image-part answer renders the image on file tap',
      (tester) async {
    final session = _FakeSession(
      listing: answer([textPart('photo.png')]),
      preview: answer([_imagePart('image/png', _onePixelPng)]),
    );
    await _pumpBrowser(tester, session);

    expect(find.byType(Image), findsNothing, reason: 'no image before the tap');

    await tester.tap(find.text('photo.png'));
    await tester.pumpAndSettle();

    expect(find.byType(Image), findsOneWidget);
  });

  // Materiality: the image must be shown on a separate overlay surface, not
  // inside the tapped file's `ListTile`. A mutant that renders the image inline
  // in the row makes the image a descendant of a `ListTile` and reds the
  // `findsNothing` below.
  testWidgets('an image result is shown in an overlay, not inline in the row',
      (tester) async {
    final session = _FakeSession(
      listing: answer([textPart('photo.png')]),
      preview: answer([_imagePart('image/png', _onePixelPng)]),
    );
    await _pumpBrowser(tester, session);

    await tester.tap(find.text('photo.png'));
    await tester.pumpAndSettle();

    expect(find.byType(Image), findsOneWidget, reason: 'the image is rendered');
    expect(
      find.descendant(of: find.byType(ListTile), matching: find.byType(Image)),
      findsNothing,
      reason:
          'the image is presented in a full-screen overlay, not inline in the file row',
    );
  });
}
