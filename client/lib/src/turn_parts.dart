// Typed parts of a live-streamed assistant turn.
//
// A turn's activity arrives as a sequence of StreamItem frames (an item's
// start / delta / stop), each keyed by a stable item_id. This model
// demultiplexes those frames into an ordered list of typed parts — text
// runs and tool calls — routed to their part by item_id. Kept free of
// Flutter so the routing is unit-testable in isolation.

import 'generated/sycophant/common/v1/common.pb.dart';

/// One typed element within a streamed assistant turn.
sealed class TurnPart {}

/// A run of streamed assistant prose.
class TextPart extends TurnPart {
  final StringBuffer text = StringBuffer();
}

/// A tool call: the tool name plus its (possibly partial) JSON arguments.
class ToolPart extends TurnPart {
  ToolPart(this.name);
  final String name;
  final StringBuffer input = StringBuffer();
}

/// Ordered typed parts for one streamed assistant turn, plus O(1) routing
/// from item_id to part index.
class StreamedParts {
  final List<TurnPart> parts = [];
  final Map<String, int> _indexByItemId = {};

  bool get isEmpty => parts.isEmpty;
  bool get isNotEmpty => parts.isNotEmpty;

  /// Apply an `ItemStart`: append a new part keyed by `itemId`. An unknown
  /// item kind is ignored (returns false) without raising — the standard's
  /// forward-compat rule. Returns true when a part was appended.
  bool applyStart(String itemId, ItemStart start) {
    final TurnPart part;
    if (start.hasText()) {
      part = TextPart();
    } else if (start.hasToolUse()) {
      part = ToolPart(start.toolUse.name);
    } else {
      return false;
    }
    _indexByItemId[itemId] = parts.length;
    parts.add(part);
    return true;
  }

  /// Apply an `ItemDelta`: append its content to the matching part's buffer.
  /// A delta for an unknown item, or one whose kind doesn't match the part,
  /// is ignored.
  void applyDelta(String itemId, ItemDelta delta) {
    final idx = _indexByItemId[itemId];
    if (idx == null) return;
    final part = parts[idx];
    if (part is TextPart && delta.hasTextDelta()) {
      part.text.write(delta.textDelta);
    } else if (part is ToolPart && delta.hasToolInputJson()) {
      part.input.write(delta.toolInputJson);
    }
  }
}
