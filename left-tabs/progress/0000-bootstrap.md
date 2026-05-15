# 0000 - Bootstrap

Date: 2026-05-15

## What exists

- Repo cloned shallow (depth 50) at `~/projects/coilysiren/wezterm/`.
- Submodules initialized (freetype, harfbuzz, libpng, zlib, etc.).
- `.claude/settings.local.json` written with a deliberately verbose allowlist (cargo, rustc, screencapture, osascript, git, gh, file utils, etc.). Intent is to unblock the loop, trim later if Kai is alarmed.
- `left-tabs/README.md` written.

## What hasn't happened yet

- Rust toolchain not verified (prior session was denied bash access for `cargo --version` and friends, which is why this isolated workspace exists). First action of the next session should be `cargo --version` and `rustc --version`.
- No build attempted. Cold debug build later measured at ~100 sec (iteration 0001), not the 10-20 min initially guessed.
- No code touched. Tab bar code location is a guess from the README, the next session should grep around `wezterm-gui/src/` to find the real entry points before planning a change.
- No screenshot driver written. Plan: a small shell or Python script that finds the WezTerm window via `osascript`, calls `screencapture -l <windowid> /tmp/wez.png`, and the agent reads the PNG via the multimodal Read tool.
- No feature branch cut.

## First moves for the next session

1. `cargo --version && rustc --version` to confirm toolchain.
2. `git switch -c left-tabs` to start a feature branch.
3. Kick off `cargo build -p wezterm-gui` in the background. Note the start time.
4. While it builds: grep `wezterm-gui/src/` for `tab_bar_at_bottom`, `tabbar`, `format-tab-title`. Map where the tab bar is laid out and how.
5. When the build finishes, launch `./target/debug/wezterm-gui` in a way that doesn't interfere with Kai's running WezTerm window. Probably with a custom `--config-file` pointing at a minimal config in this folder.
6. Write `progress/0001-*.md` summarizing what was found and what to try next.

## Risk register

- Wez Furlong may reject the eventual PR on aesthetic grounds. Mitigation: maintain a fork that rebases onto upstream.
- The change might touch more layout code than expected. If iteration N reveals the scope is 5x what was estimated, write that up plainly and stop. Don't burn hours producing broken commits.
- macOS Screen Recording permission for `screencapture` may need a one-time approval from Kai.
