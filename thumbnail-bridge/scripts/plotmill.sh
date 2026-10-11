#!/usr/bin/env bash
# The bridge's command-line tool (convert / inspect / edit-text / export / doctor).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/dev-env.sh
source "$ROOT/scripts/dev-env.sh"
export ORT_DISABLE_TELEMETRY=1
BIN="$CARGO_TARGET_DIR/release/plotmill"
[ -x "$BIN" ] || BIN="$CARGO_TARGET_DIR/debug/plotmill"
[ -x "$BIN" ] || { echo "plotmill is not built yet: run scripts/setup.sh" >&2; exit 1; }
exec "$BIN" "$@"
