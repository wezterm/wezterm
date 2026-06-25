#!/bin/bash
# Update the bundled conhost assets (conpty.dll, OpenConsole.exe and
# OpenConsoleProxy.dll) from a Microsoft Windows Terminal release.
#
# By default the latest stable release is used; pass a release tag (eg.
# v1.24.11321.0) as the first argument to pin to a specific release.
#
# eg:
#   ./assets/windows/conhost/update.sh
#   ./assets/windows/conhost/update.sh v1.24.11321.0
set -x
set -e

cd "$(git rev-parse --show-toplevel)"

TAG="${1:-latest}"
DEST=assets/windows/conhost
WORK=/tmp/wt-conhost-update

# Resolve the release tag and asset URLs.  The ConPTY nupkg carries a
# date-based version (eg. 1.24.260512001) that is not derivable from the
# release tag, so we have to consult the release assets list.
if [[ "$TAG" == "latest" ]] ; then
  API_URL=https://api.github.com/repos/microsoft/terminal/releases/latest
else
  API_URL=https://api.github.com/repos/microsoft/terminal/releases/tags/$TAG
fi

RELEASE_JSON=$(curl -sSL "$API_URL")
TAG=$(printf '%s' "$RELEASE_JSON" \
  | sed -nE 's/.*"tag_name":[[:space:]]*"([^"]+)".*/\1/p' \
  | head -1)
X64_URL=$(printf '%s' "$RELEASE_JSON" \
  | grep -oE 'https://[^"]*releases/download/[^"]*_x64\.zip' \
  | head -1)
NUPKG_URL=$(printf '%s' "$RELEASE_JSON" \
  | grep -oE 'https://[^"]*releases/download/[^"]*ConPTY[^"]*\.nupkg' \
  | head -1)

test -n "$TAG"
test -n "$X64_URL"
test -n "$NUPKG_URL"

rm -rf "$WORK"
mkdir -p "$WORK"
curl -sSL -o "$WORK/wt_x64.zip" "$X64_URL"
curl -sSL -o "$WORK/conpty.nupkg" "$NUPKG_URL"

# The x64 zip is the unpacked MSIX layout: payload files sit flat under
# a terminal-<version>/ directory.  OpenConsole.exe and its COM proxy
# stub both ship here.
unzip -j -o "$WORK/wt_x64.zip" \
  'terminal-*/OpenConsole.exe' \
  'terminal-*/OpenConsoleProxy.dll' \
  -d "$DEST"

# conpty.dll is the ConPTY client library; it ships only in the ConPTY
# nupkg.  It must match the OpenConsole.exe release because conpty.dll
# spawns OpenConsole.exe as its ConPTY host at runtime.
unzip -j -o "$WORK/conpty.nupkg" \
  'runtimes/win-x64/native/conpty.dll' \
  -d "$DEST"

echo "Updated $DEST to $TAG"
