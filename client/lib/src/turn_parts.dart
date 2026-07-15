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

/// Groups streamed sub-agent frames under the active parent turn by the
/// parent<->child correlation identifier. A frame belongs to this parent iff
/// its `parentConversationId` matches the active parent (a non-empty link);
/// grouped frames are bucketed by the child's own `conversationId` so each
/// dispatched sub-agent renders as its own collapsible group. Flutter-free so
/// the routing is unit-testable in isolation.
class SubagentGroups {
  SubagentGroups({required this.parentConversationId});

  /// The active turn's conversation id — a frame nests here only when its
  /// parent link points at this id.
  final String parentConversationId;

  final Map<String, StreamedParts> _byChild = {};
  final List<String> _order = [];
  final Map<String, String> _nameByChild = {};

  /// Child conversation ids seen, in first-seen order.
  Iterable<String> get childConversationIds => _order;

  /// The streamed parts routed to a given child group.
  StreamedParts partsFor(String childConversationId) =>
      _byChild[childConversationId] ?? StreamedParts();

  /// The operator-authored name observed for a child group, or null if none
  /// was seen. First non-empty name wins; later empty frames don't clobber it.
  String? nameFor(String childConversationId) => _nameByChild[childConversationId];

  /// Route a frame. Returns true iff it was grouped as a sub-agent child of
  /// the active parent — i.e. its parent link is non-empty AND matches this
  /// parent. A top-level frame (empty parent link) or a frame for a different
  /// parent is not grouped (returns false).
  bool apply(StreamItem item) {
    if (item.parentConversationId.isEmpty) return false;
    if (item.parentConversationId != parentConversationId) return false;
    final child = item.conversationId;
    if (!_byChild.containsKey(child)) {
      _byChild[child] = StreamedParts();
      _order.add(child);
    }
    if (item.agentName.isNotEmpty && !_nameByChild.containsKey(child)) {
      _nameByChild[child] = item.agentName;
    }
    final parts = _byChild[child]!;
    if (item.hasStart()) {
      parts.applyStart(item.itemId, item.start);
    } else if (item.hasDelta()) {
      parts.applyDelta(item.itemId, item.delta);
    }
    return true;
  }
}
