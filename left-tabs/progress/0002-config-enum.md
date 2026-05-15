# 0002 - Config enum landed, sanity loop confirmed

Date: 2026-05-15

## What happened

- Captured baseline screenshot via `scripts/iter.sh`. Window with horizontal tab bar reading "1: zsh x" at top. Fullscreen fallback path was used because the AppleScript window-rect query returned empty, but the WezTerm window is plainly visible in the capture so the loop works for now.
- Added `TabBarPosition` enum (`Top` / `Bottom` / `Left`) to `config/src/config.rs` with `FromDynamic`/`ToDynamic`, defaulting to `Top`. Added a sibling `tab_bar_position: TabBarPosition` field next to `tab_bar_at_bottom`.
- Incremental build: 17 seconds. Clean.
- Re-ran the screenshot loop, post-enum-add window is visually identical to baseline. Config-only change is safe to keep.

## What's next

The user-visible behavior is still identical, the enum is dormant. The plan for iteration 0003:

1. Add a `TabBarPosition::resolve(config) -> TabBarPosition` (or method on `ConfigHandle`) that returns the effective position: if `tab_bar_position` is non-default it wins, else fall back to `tab_bar_at_bottom`. This is the single resolution point every consumer will eventually use.
2. Introduce a `TabBarInsets { top, bottom, left, right: f32 }` struct (likely in `wezterm-gui/src/termwindow/render/tab_bar.rs`) with a method on `TermWindow` that returns the current insets given the active config + measured bar dimensions.
3. Refactor the 12-odd `tab_bar_at_bottom` branches to read from `TabBarInsets` instead. Each becomes 3 lines and treats top/bottom/left uniformly.
4. Implement the vertical paint path in `fancy_tab_bar.rs` (mirror of horizontal, stacking elements column-direction).
5. Vertical bar width: derive from longest tab title or use a new `tab_bar_pixel_width` knob. Start with a hardcoded sensible default (~200 px), promote to config later.

## Risk update

- Background command output is reaching the agent fine, but a 17-second incremental build is the floor. The first full refactor pass might trigger a wider recompile of `wezterm-gui` (couple minutes).
- The capture path falling back to fullscreen is OK because the test window opens top-left, but if Kai resizes her desktop the capture will still find it. The AppleScript rect query was returning empty even though System Events sees the process. Acceptable for now, not worth a yak-shave.
