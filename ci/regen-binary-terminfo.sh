#!/usr/bin/env bash

# Regenerate the compiled terminfo database from its text source.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# tic lays entries out as <dir>/<first-letter-of-name>/<name>,
# so this writes termwiz/data/w/wezterm.
tic -x -o "$repo_root/termwiz/data" "$repo_root/termwiz/data/wezterm.terminfo"
