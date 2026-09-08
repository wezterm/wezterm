#!/bin/bash
set -euo pipefail

# ensure we're running from the repo root
start_dir="$PWD"
repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

# ensure required commands are available
for cmd in curl unzip cargo ucd-generate; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
      echo "error: missing required command: $1" >&2
      exit 1
    fi
done

tmp_dir="$(mktemp -d 2>/dev/null || mktemp -d -t wezterm-unicode-update)"

cleanup() {
  rm -rf "$tmp_dir"
  cd "$start_dir"
}
trap cleanup EXIT INT TERM

echo "==> Regenerating emoji_variation.rs"
(
  cd "wezterm-char-props/codegen"
  cargo run > ../src/emoji_variation.rs
)

echo "==> Updating widechar_width.rs from ridiculousfish/widecharwidth"
wide_url="https://raw.githubusercontent.com/ridiculousfish/widecharwidth/master/widechar_width.rs"
wide_path="$tmp_dir/widechar_width.rs"
curl -fsSL "$wide_url" -o "$wide_path"

# sanity check: ensure file looks like the expected upstream content
if ! grep -q "widechar_width.rs" "$wide_path"; then
  echo "error: downloaded widechar_width.rs does not look like the expected upstream file: $wide_url" >&2
  exit 1
fi

cp "$wide_path" "wezterm-char-props/src/widechar_width.rs"

echo "==> Downloading latest Unicode UCD data"
ucd_zip="$tmp_dir/UCD.zip"
curl -fsSL "https://www.unicode.org/Public/UCD/latest/ucd/UCD.zip" -o "$ucd_zip"
(
  cd "$tmp_dir"
  unzip -q "$ucd_zip"
)

if [ ! -f "$tmp_dir/PropList.txt" ]; then
  echo "error: UCD zip did not contain PropList.txt, cannot run ucd-generate in $tmp_dir" >&2
  exit 1
fi

echo "==> Regenerating emoji_presentation.rs"
emoji_presentation_path="$tmp_dir/emoji_presentation.rs"
(
  cd "$tmp_dir"
  ucd-generate property-bool . --include Emoji_Presentation --trie-set > "$emoji_presentation_path"
)

# sanity check: ensure generated file references expected property name
if ! grep -q "Emoji_Presentation" "$emoji_presentation_path"; then
  echo "error: generated emoji_presentation.rs did not contain Emoji_Presentation, ucd-generate output unexpected" >&2
  exit 1
fi

cp "$emoji_presentation_path" "wezterm-char-props/src/emoji_presentation.rs"

echo "==> Formatting"
if ! cargo +nightly fmt --version >/dev/null 2>&1; then
  echo "error: rustfmt is not available in the nightly toolchain." >&2
  echo "Install it with:" >&2
  echo "  rustup toolchain install nightly" >&2
  echo "  rustup component add rustfmt --toolchain nightly" >&2
  exit 1
fi

cargo +nightly fmt

echo "==> Done"
