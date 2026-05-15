# Left-Tabs

A long-running attempt to land a vertical (left-side) tab bar in WezTerm.

## Goal

Make WezTerm's tab strip render down the left edge of the window instead of across the top, as a config option (`config.tab_bar_position = "Left"` or similar). Existing horizontal behavior must continue to work unchanged when the option is unset or set to `Top` / `Bottom`.

## Why this exists

Kai moved from Warp to WezTerm because Warp piled on UI elements until the actual terminal stopped being keyboard-driven. WezTerm is the right minimalism, but vertical tabs is the one Warp affordance worth missing. There's no plugin hook for a custom vertical strip, the tab bar is rendered as a horizontal strip with layout assumptions baked into `wezterm-gui`. Going vertical means refactoring window layout, resize logic, hit-testing, and the tab format API.

Long-standing issue thread: wez/wezterm#1180 and friends. Wez Furlong has historically been lukewarm but not hostile, the realistic shape is "land a clean PR or maintain a fork."

## Loop shape

Long-horizon autonomous engineering. Edit Rust, build (debug, incremental), launch a clean test instance of the new binary, screenshot the window, read the screenshot, decide the next edit, log progress, repeat.

Iteration cost estimate: cold debug build on Kai's Mac measured at ~100 seconds (2026-05-15, registry warm). Incremental builds 30 seconds to several minutes depending on which crate changed. A four-hour session is probably 30 to 60 iterations, not hundreds. Spend each iteration carefully.

## Layout of this folder

- `README.md` - this file, the durable orientation doc.
- `progress/` - one markdown file per iteration or per session, numbered or dated. Newest file = current state. Older files = audit trail. A fresh agent picking up the work after compaction should read the latest 2-3 entries before doing anything else.

## Key paths

- Repo root: `~/projects/coilysiren/wezterm/`
- GUI crate (where layout lives): `wezterm-gui/`
- Tab bar code starts at: `wezterm-gui/src/tabbar.rs`, `wezterm-gui/src/termwindow/render/` (look around there).
- Config schema: `config/src/config.rs`
- Test binary after build: `target/debug/wezterm-gui`
- Permissions for the loop: `.claude/settings.local.json` at repo root.

## Constraints on the agent

- Write progress entries every iteration. Even "build failed, nothing rendered" is a useful entry, because the next agent (or the next-you after compaction) needs to know what was tried.
- Don't commit to the fork's main branch carelessly. Use a feature branch.
- If you discover the approach is wrong (e.g., refactor scope balloons past what's realistic), say so in `progress/` and stop. A clean "this won't work because X" entry is more valuable than 50 broken commits.
- Voice rules from `~/projects/coilysiren/agentic-os-kai/AGENTS.md` still apply to anything Kai will read: no em-dashes, no italics, no semicolons in prose, no prose tables, she/her pronouns. Internal code comments can be normal Rust style.

## Bootstrap for a fresh session

1. Read this README.
2. Read the newest 2-3 files in `progress/`.
3. Check `git status` and `git log --oneline -10` to see what's been done.
4. Continue from there.

## Status

Not started. First iteration will be the cold debug build, expect ~20 minutes of wall time before any code can run.
