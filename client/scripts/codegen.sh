#!/usr/bin/env bash
# Regenerate Dart gRPC stubs from the canonical .proto in
# crates/tightbeam-proto. Run when the proto changes; the generated files
# are committed (no automatic build-time regen) so a fresh clone is
# immediately buildable without protoc.
#
# Prereqs: `dart pub global activate protoc_plugin` and `protoc` on PATH.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
PROTO_ROOT="$REPO_ROOT/crates/tightbeam-proto/proto"
OUT_DIR="$REPO_ROOT/client/lib/src/generated"

export PATH="$PATH:$HOME/.pub-cache/bin"

mkdir -p "$OUT_DIR"
protoc \
  --dart_out=grpc:"$OUT_DIR" \
  -I "$PROTO_ROOT" \
  "$PROTO_ROOT/tightbeam/v1/tightbeam.proto"

echo "regenerated Dart stubs in $OUT_DIR"
