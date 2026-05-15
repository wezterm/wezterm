#!/usr/bin/env bash
# Launch the freshly-built wezterm-gui binary in isolation, screencapture
# its window via CGWindowID, then exit it. Usage: iter.sh <out-png-path>
set -euo pipefail

REPO="/Users/kai/projects/coilysiren/wezterm"
BIN="$REPO/target/debug/wezterm-gui"
CONFIG="$REPO/left-tabs/scripts/test-config.lua"
FIND_WIN="$REPO/left-tabs/scripts/find_window.swift"
OUT="${1:-/tmp/wez-left-tabs.png}"

RUNTIME=$(mktemp -d /tmp/wezterm-left-tabs-runtime.XXXXXX)
export XDG_RUNTIME_DIR="$RUNTIME"
export WEZTERM_CONFIG_FILE="$CONFIG"

"$BIN" --config-file "$CONFIG" start >"$RUNTIME/stdout.log" 2>"$RUNTIME/stderr.log" &
PID=$!

# Poll for a real content window to appear.
WID=""
for i in $(seq 1 50); do
  WID=$(swift "$FIND_WIN" "$PID" 2>/dev/null || true)
  if [[ -n "$WID" ]]; then break; fi
  sleep 0.2
done

sleep 0.8

if [[ -n "$WID" ]]; then
  # screencapture -l grabs the CG window content regardless of z-order
  # or what's on top of it. Perfect for headless render checks.
  screencapture -o -x -l "$WID" "$OUT"
else
  screencapture -o -x "$OUT"
fi

kill "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true
rm -rf "$RUNTIME"

echo "wrote: $OUT"
echo "wid: ${WID:-<fallback-fullscreen>}"
