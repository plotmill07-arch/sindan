#!/usr/bin/env bash
# Start PhotoCraft (with File › Import › AI Thumbnails…). Extra arguments go to PhotoCraft.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/dev-env.sh
source "$ROOT/scripts/dev-env.sh"
# Belt and braces: the pinned ONNX Runtime has no telemetry uploader, and the bridge refuses one
# that has; this also switches it off for any other build someone drops in.
export ORT_DISABLE_TELEMETRY=1
BIN="$CARGO_TARGET_DIR/release/photocraft"
[ -x "$BIN" ] || BIN="$CARGO_TARGET_DIR/debug/photocraft"
[ -x "$BIN" ] || { echo "PhotoCraft is not built yet: run scripts/setup.sh" >&2; exit 1; }
exec "$BIN" "$@"
