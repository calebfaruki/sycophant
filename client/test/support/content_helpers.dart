// Shared builders for tool-result content used across browser-pane tests.

import 'package:sycophant_client/src/generated/sycophant/common/v1/common.pb.dart';

ContentBlock textPart(String s) => ContentBlock()..text = (TextBlock()..text = s);

CallToolResponse answer(List<ContentBlock> parts) =>
    CallToolResponse()..content.addAll(parts);
