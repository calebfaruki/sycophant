import 'generated/sycophant/common/v1/common.pb.dart';

/// Join the text of a tool answer's text parts — the client's mirror of the
/// Rust text read. Image parts are skipped; a caller that renders images walks
/// the parts itself with [firstImagePart].
String joinTextParts(Iterable<ContentBlock> content) =>
    content.where((b) => b.hasText()).map((b) => b.text.text).join('\n');

/// The first image part in a tool answer, or `null` when the answer is
/// text-only. The client stays tool-agnostic: it renders whatever image part
/// comes back, regardless of which tool produced it.
ImageBlock? firstImagePart(Iterable<ContentBlock> content) {
  for (final block in content) {
    if (block.hasImage()) return block.image;
  }
  return null;
}
