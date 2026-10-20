#!/usr/bin/env bash
# Integration test for the X11 keyboard path: how the kitty keyboard
# protocol encodes keypad keys depending on the NumLock state.
#
# Regression test for "x11: keep latched and locked modifiers in the
# merged key state" (window/src/os/x11/keyboard.rs): the X11 event mask
# only carries effective modifiers, so latched/locked components must be
# merged in from the cached xcb state, otherwise the LED indicators stay
# dark and keypad digits get encoded as End/Delete/Insert/Home.
#
# The rig runs an isolated Xvfb and injects raw keycodes through XTEST;
# keyname injection via xdotool is unusable here because it toggles
# NumLock itself while synthesizing the keysym.  Assertions grep the
# wezterm trace log emitted by the same branch that writes the encoded
# keys to the pane (debug_key_events = true), so the evidence is
# source-level rather than a screen scrape.
#
# Usage: ci/test-x11-keyboard.sh [path-to-wezterm-gui]
# Env:   XTEST_DISPLAY  display to use for the throwaway Xvfb (default :83)
# Needs: Xvfb, xdotool, xev (x11-utils), python3 (ctypes + libXtst)

set -e

WT=${1:-target/release/wezterm-gui}
DISP=${XTEST_DISPLAY:-:83}
WORK=$(mktemp -d /tmp/wezterm-x11-kbtest.XXXXXX)
XVFB_PID=""
XEV_PID=""
GUI_PID=""
PASS_COUNT=0

cleanup() {
  # only ever kill processes this script started, by recorded PID
  for pid in $GUI_PID $XEV_PID $XVFB_PID; do
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
    fi
  done
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*"
  echo "evidence kept in: $WORK"
  exit 1
}

pass() {
  echo "PASS: $*"
  PASS_COUNT=$((PASS_COUNT + 1))
}

assert_log_has() { # <file> <fixed string> <description>
  if grep -qF -- "$2" "$1"; then
    pass "$3"
  else
    fail "$3: '$2' not found in $1"
  fi
}

assert_log_lacks() { # <file> <fixed string> <description>
  if grep -qF -- "$2" "$1"; then
    fail "$3: unexpected '$2' in $1"
  else
    pass "$3"
  fi
}

[ -x "$WT" ] || fail "wezterm-gui binary not executable: $WT (build one with: cargo build --release -p wezterm-gui)"
for tool in Xvfb xdotool xev python3; do
  command -v "$tool" >/dev/null 2>&1 || fail "missing tool: $tool (on debian/ubuntu: apt-get install -y xvfb xdotool x11-utils)"
done

cat >"$WORK/xkey.py" <<'PYEOF'
#!/usr/bin/env python3
"""Inject raw keycodes through XTEST (ctypes straight into libXtst).

Keyname injection (eg: xdotool key KP_1) toggles NumLock while
synthesizing the keysym, so the key arrives with the lock flipped and
the rig cannot control the state it is testing.  Each keycode is
pressed for 60ms and released.

usage: xkey.py <display> <keycode> [<keycode> ...]"""
import ctypes
import sys
import time


class X(ctypes.Structure):
    pass


X_p = ctypes.POINTER(X)
libX11 = ctypes.cdll.LoadLibrary("libX11.so.6")
libXtst = ctypes.cdll.LoadLibrary("libXtst.so.6")
libX11.XOpenDisplay.restype = X_p
libX11.XOpenDisplay.argtypes = [ctypes.c_char_p]
libX11.XFlush.argtypes = [X_p]
libXtst.XTestFakeKeyEvent.argtypes = [X_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]

disp, codes = sys.argv[1], [int(a) for a in sys.argv[2:]]
dpy = libX11.XOpenDisplay(disp.encode())
if not dpy:
    sys.exit(f"XOpenDisplay({disp}) failed")
for kc in codes:
    libXtst.XTestFakeKeyEvent(dpy, kc, True, 0)
    libX11.XFlush(dpy)
    time.sleep(0.06)
    libXtst.XTestFakeKeyEvent(dpy, kc, False, 0)
    libX11.XFlush(dpy)
    time.sleep(0.15)
PYEOF

cat >"$WORK/conf.lua" <<'LUAEOF'
-- Assert on the kitty encoding explicitly so the test does not depend
-- on the build's default for enable_kitty_keyboard
return {
  debug_key_events = true,
  enable_kitty_keyboard = true,
}
LUAEOF

cat >"$WORK/sniff.sh" <<'SNIFFEOF'
#!/bin/bash
# Push the kitty REPORT_ALL_KEYS_AS_ESCAPE_CODES flag (bit 8) the way a
# real TUI would, then capture raw bytes.  The timeout closes the pane
# so the window shuts down gracefully and the trace log gets flushed.
printf '\033[>8u'
stty raw -echo
timeout 14 cat > "$1"
SNIFFEOF
chmod +x "$WORK/sniff.sh"

# Keycodes: KP_1=87 KP_5=84 KP_Decimal=91 KP_Enter=104 KP_0=90 KP_7=79,
# NumLock=77, 'm'=58 (plain key to prove the lock via its event state)
KP_KEYS="87 84 91 104 90 79"

start_xvfb() { # <label>
  Xvfb "$DISP" -screen 0 1280x800x24 >"$WORK/xvfb-$1.log" 2>&1 &
  XVFB_PID=$!
  sleep 1.2
}

teardown_xvfb() {
  if [ -n "$XVFB_PID" ] && kill -0 "$XVFB_PID" 2>/dev/null; then
    kill "$XVFB_PID" 2>/dev/null || true
  fi
  XVFB_PID=""
  GUI_PID=""
  sleep 0.3
}

inject() {
  python3 "$WORK/xkey.py" "$DISP" "$@"
}

launch_wezterm() { # <label>
  WEZTERM_CONFIG_FILE="$WORK/conf.lua" WEZTERM_LOG=trace \
    "$WT" start --always-new-process -- "$WORK/sniff.sh" "$WORK/sniff-$1.bin" \
    >"$WORK/wt-stdout-$1.log" 2>&1 &
  local win=""
  for _ in $(seq 1 80); do
    sleep 0.25
    for w in $(xdotool search --class wezterm 2>/dev/null); do win=$w; break; done
    [ -n "$win" ] && break
  done
  [ -n "$win" ] || fail "$1: no wezterm window appeared on $DISP"
  # the window's _NET_WM_PID attributes the GUI process exactly; pgrep
  # by name could match an unrelated wezterm the tester is running
  GUI_PID=$(xdotool getwindowpid "$win" 2>/dev/null || true)
  xdotool windowfocus "$win" 2>/dev/null || true
  sleep 1.0
}

stop_wezterm() {
  # windowkill closes the pane gracefully so the trace log is flushed;
  # killing the process (or the X server) would lose the tail
  local win=""
  for w in $(xdotool search --class wezterm 2>/dev/null); do win=$w; break; done
  if [ -n "$win" ]; then
    xdotool windowkill "$win" 2>/dev/null || true
  fi
  sleep 1.5
}

leg_numlock_on() {
  local label=on
  local log=$WORK/wt-stdout-$label.log
  start_xvfb "$label"
  launch_wezterm "$label"
  inject 77 # NumLock on
  sleep 0.3
  for kc in $KP_KEYS; do
    inject "$kc"
    sleep 0.45
  done
  sleep 1.0
  stop_wezterm
  teardown_xvfb

  echo "-- leg 1: NumLock locked while wezterm runs"
  assert_log_has "$log" '"\u{1b}[57400;129u"' "KP_1 encodes as digit 1 with the numlock modifier"
  assert_log_has "$log" '"\u{1b}[57404;129u"' "KP_5 encodes as digit 5 with the numlock modifier"
  assert_log_has "$log" '"\u{1b}[57399;129u"' "KP_0 encodes as digit 0"
  assert_log_has "$log" '"\u{1b}[57406;129u"' "KP_7 encodes as digit 7"
  assert_log_has "$log" '"\u{1b}[57414;129u"' "KP_Enter keeps its functional code"
  assert_log_has "$log" 'leds: NUM_LOCK' "key events carry the NUM_LOCK led"
  assert_log_lacks "$log" '"\u{1b}[57424;1u"' "KP_1 no longer encodes as End"
}

leg_numlock_off() {
  local label=off
  local log=$WORK/wt-stdout-$label.log
  start_xvfb "$label"
  launch_wezterm "$label"
  for kc in $KP_KEYS; do
    inject "$kc"
    sleep 0.45
  done
  sleep 1.0
  stop_wezterm
  teardown_xvfb

  echo "-- leg 2: NumLock unlocked (control leg: the rig must produce navigation codes)"
  assert_log_has "$log" '"\u{1b}[57424;1u"' "KP_1 encodes as End"
  assert_log_has "$log" '"\u{1b}[E"' "KP_5 encodes as cursor-down"
  assert_log_has "$log" 'leds: (empty)' "no leds reported"
}

leg_prelock() {
  local label=prelock
  local log=$WORK/wt-stdout-$label.log
  start_xvfb "$label"

  # A focused client must exist for the server to process XTEST keys at
  # all; xev doubles as the witness of the lock.  It has to stay alive
  # for the whole leg: killing the focused client resets the X server's
  # locked modifiers, which would silently break the scenario under test
  stdbuf -oL xev >"$WORK/xev-$label.log" 2>&1 &
  XEV_PID=$!
  local xev_log=$WORK/xev-$label.log
  for _ in $(seq 1 20); do
    [ -s "$xev_log" ] && break
    sleep 0.25
  done
  [ -s "$xev_log" ] || fail "prelock: xev produced no output"
  local xev_win
  read -r _ _ _ xev_win _ <"$xev_log"
  xev_win=${xev_win%,} # strip the trailing comma: 0x200001, -> 0x200001
  xev_win=$((xev_win)) # hex window id to decimal for xdotool
  xdotool windowfocus "$xev_win" 2>/dev/null || true
  sleep 0.3
  inject 77 # lock NumLock before wezterm starts
  sleep 0.3
  inject 58 # plain 'm': its event state proves the lock is really set
  sleep 0.3

  launch_wezterm "$label"
  inject 87
  sleep 0.45
  inject 84
  sleep 0.45
  sleep 1.0
  stop_wezterm
  if [ -n "$XEV_PID" ] && kill -0 "$XEV_PID" 2>/dev/null; then
    kill "$XEV_PID" 2>/dev/null || true
  fi
  XEV_PID=""
  teardown_xvfb

  echo "-- leg 3: NumLock locked before wezterm started (startup seeding)"
  assert_log_has "$xev_log" 'state 0x10' "the lock was really engaged before wezterm started"
  assert_log_has "$log" '"\u{1b}[57400;129u"' "first keypad key encodes as digit 1"
  assert_log_has "$log" '"\u{1b}[57404;129u"' "KP_5 encodes as digit 5"

  # the lock must come from the startup seeding: no XkbStateNotify may
  # have arrived before the first key event
  local first line
  first=$(grep -n -m1 -F 'merge_current_xcb_modifiers(selected)' "$log" | cut -d: -f1)
  [ -n "$first" ] || fail "prelock: no merge trace found in $log"
  if head -n "$first" "$log" | grep -qF 'update_state('; then
    fail "prelock: an XkbStateNotify arrived before the first key; not the pure seeding path"
  fi
  pass "no XkbStateNotify before the first key (pure startup seeding)"
  line=$(grep -m1 -F 'merge_current_xcb_modifiers(selected)' "$log" || true)
  case "$line" in
    *'locked_mods: 16'*)
      pass "first merged state carries locked_mods 16 from the seed"
      ;;
    *)
      fail "prelock: first merge does not carry locked_mods 16: $line"
      ;;
  esac
}

export DISPLAY=$DISP

echo "== wezterm X11 keyboard integration test"
echo "   binary:  $WT"
echo "   display: $DISP"
echo "   workdir: $WORK"

leg_numlock_on
leg_numlock_off
leg_prelock

echo "== all $PASS_COUNT assertions passed"
rm -rf "$WORK"
trap - EXIT
