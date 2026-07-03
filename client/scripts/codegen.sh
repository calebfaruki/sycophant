#!/usr/bin/env bash
# Regenerate Dart gRPC stubs from the canonical .proto in
# crates/tightbeam-proto (plus its proto-common dependency). Run when the
# proto changes; the generated files are committed (no automatic
# build-time regen) so a fresh clone is immediately buildable without
# protoc.
#
# The external client surface lives on tightbeam.v1.TightbeamGateway,
# whose RPCs carry sycophant.common.v1.* messages defined in
# proto-common. Both protos are passed as inputs so the dart plugin emits
# the shared message file alongside the gateway client.
#
# Prereqs: `dart pub global activate protoc_plugin` and `protoc` on PATH.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
TIGHTBEAM_PROTO_ROOT="$REPO_ROOT/crates/tightbeam-proto/proto"
COMMON_PROTO_ROOT="$REPO_ROOT/crates/proto-common/proto"
OUT_DIR="$REPO_ROOT/client/lib/src/generated"

export PATH="$PATH:$HOME/.pub-cache/bin"

mkdir -p "$OUT_DIR"
protoc \
  --dart_out=grpc:"$OUT_DIR" \
  -I "$TIGHTBEAM_PROTO_ROOT" \
  -I "$COMMON_PROTO_ROOT" \
  "$TIGHTBEAM_PROTO_ROOT/tightbeam/v1/tightbeam.proto" \
  "$COMMON_PROTO_ROOT/sycophant/common/v1/common.proto"

echo "regenerated Dart stubs in $OUT_DIR"
