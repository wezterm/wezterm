#!/bin/bash
# Build a complete WezTerm.app bundle from local release binaries.
# Mirrors ci/deploy.sh macos section, minus CI certificate signing
# (we ad-hoc sign at the end so Gatekeeper/quarantine doesn't block it).
set -euo pipefail

cd "$(dirname "$0")"

TARGET_DIR=target
OUT_DIR=dist
APP=WezTerm.app

echo "==> Verifying release binaries exist"
for bin in wezterm wezterm-mux-server wezterm-gui strip-ansi-escapes; do
  if [[ ! -f $TARGET_DIR/release/$bin ]]; then
    echo "ERROR: missing $TARGET_DIR/release/$bin"
    echo "       run: cargo build --release -p wezterm -p wezterm-mux-server -p wezterm-gui -p strip-ansi-escapes"
    exit 1
  fi
done

echo "==> Cleaning previous output"
rm -rf $OUT_DIR
mkdir -p $OUT_DIR

echo "==> Copying app bundle template from assets/macos/"
cp -r assets/macos/$APP $OUT_DIR/

# Omit MetalANGLE dylibs (matches official build: CGL is used on macOS,
# and on Apple Silicon CGL is implemented via Metal anyway).
rm -f $OUT_DIR/$APP/*.dylib

echo "==> Creating Contents/MacOS and Contents/Resources"
mkdir -p $OUT_DIR/$APP/Contents/MacOS
mkdir -p $OUT_DIR/$APP/Contents/Resources

echo "==> Copying shell-integration and shell-completion"
cp -r assets/shell-integration/* $OUT_DIR/$APP/Contents/Resources/
cp -r assets/shell-completion $OUT_DIR/$APP/Contents/Resources/

echo "==> Compiling terminfo (wezterm)"
tic -xe wezterm -o $OUT_DIR/$APP/Contents/Resources/terminfo termwiz/data/wezterm.terminfo

echo "==> Copying release binaries"
for bin in wezterm wezterm-mux-server wezterm-gui strip-ansi-escapes; do
  cp $TARGET_DIR/release/$bin $OUT_DIR/$APP/Contents/MacOS/$bin
  echo "    - $bin ($(du -h $TARGET_DIR/release/$bin | awk '{print $1}'))"
done

echo "==> Ad-hoc codesign (no Apple certificate available locally)"
/usr/bin/codesign --force --options runtime \
  --entitlements ci/macos-entitlement.plist --deep --sign - $OUT_DIR/$APP/

echo "==> Verifying signature"
/usr/bin/codesign --verify --verbose=2 $OUT_DIR/$APP/ 2>&1 || true

echo
echo "==> Done. Bundle at: $OUT_DIR/$APP"
echo "    Install with:   cp -R $OUT_DIR/$APP /Applications/"
echo "    (quit WezTerm first: pkill -f wezterm-gui)"
echo "    Or run side-by-side as WezTerm-dev: cp -R $OUT_DIR/$APP /Applications/WezTerm-dev.app"
