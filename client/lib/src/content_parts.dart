import 'generated/sycophant/common/v1/common.pb.dart';

/// Join the text of a tool answer's text parts — the client's mirror of the
/// Rust text read. Image parts are skipped; a caller that renders images walks
/// the parts itself.
String joinTextParts(Iterable<ContentBlock> content) =>
    content.where((b) => b.hasText()).map((b) => b.text.text).join('\n');
