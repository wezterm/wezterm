# 0003 - Vertical tabs rendering end-to-end

Date: 2026-05-15

## Outcome

`config.tab_bar_position = 'Left'` paints a vertical tab strip down
the left edge of the WezTerm window. The terminal pane inset is
correct, the tab strip is full window height, the active tab is
highlighted, the `+` new-tab affordance renders below the tabs.

Screenshots in this iteration:

- `/tmp/wez-post-refactor.png` - tab_bar_position = Top, identical to baseline (no visual regression from the refactor).
- `/tmp/wez-bottom.png` - tab_bar_position = Bottom, bar at bottom of window, prompt at top.
- `/tmp/wez-left-final.png` - tab_bar_position = Left, the new mode. Vertical strip on the left, pane content offset to start after it.

All three modes verified in the same binary build.

## What changed

### Capture loop (`scripts/iter.sh`)

The previous AppleScript rect lookup was returning the wrong window
position whenever Kai's daily WezTerm session was also running, so
`screencapture` was grabbing the desktop region behind the test
window. Rewrote the script to use a small Swift helper
(`scripts/find_window.swift`) that walks `CGWindowListCopyWindowInfo`,
filters to the launched PID's largest layer-0 window, and prints the
CGWindowID. `screencapture -l <wid>` then grabs the test window's
own backing buffer regardless of z-order. The headless render loop
is now reliable even with other WezTerm windows around.

### Config (`config/src/config.rs`)

Added `TabBarPosition` enum (`Top` / `Bottom` / `Left`) and a sibling
`tab_bar_position` field. Resolution rule: `tab_bar_position` wins
when non-default; otherwise falls back to legacy `tab_bar_at_bottom`.
Legacy users see no behavior change.

### Geometry refactor

Introduced `TabBarInsets { top, bottom, left, right }` plus the
helpers `tab_bar_insets()`, `tab_bar_pixel_width()`, and
`effective_tab_bar_position()` on `TermWindow`. Refactored every site
that previously special-cased `tab_bar_at_bottom`:

- `render/pane.rs` (paint + build_pane) - background_rect, content_rect, left_pixel_x all add `insets.left`.
- `render/split.rs` - split divider offset includes `insets.left`.
- `render/mod.rs` - `horizontal_gap` / `vertical_gap` use `insets.horizontal()` / `insets.vertical()`.
- `mouseevent.rs` - first_line_offset and first_col_offset come from insets.
- `paneselect.rs`, `palette.rs`, `charselect.rs` - modal overlays read insets.
- `termwindow/mod.rs` - window construction adds `tab_bar_h_inset` to pixel_width when applicable, hovering_in_tab_bar branches on position, text cursor rect adds left inset.
- `termwindow/resize.rs` - `apply_dimensions` and the scale path both subtract horizontal insets from `avail_width` and reflect them in `Dimensions`.
- `resize_increment_calculator.rs` - new `tab_bar_width` field carried through into `base_width`.

### Vertical render path (`render/fancy_tab_bar.rs`)

`build_fancy_tab_bar` now dispatches to a new
`build_fancy_tab_bar_vertical` when position is Left. The vertical
builder constructs each tab as a `DisplayType::Block` child of an
outer container sized `tab_bar_pixel_width` x `pixel_height`. The
box model already advances `y_coord` between Block children, so
stacking is free once the display mode flips. Right-floated and
status items are dropped for the first cut; they can be plumbed in
once the basic flow is solid.

`paint_tab_bar` (non-fancy path) and `paint_fancy_tab_bar`
(horizontal y-translate) now read `effective_tab_bar_position()`
instead of `tab_bar_at_bottom` directly, which is what fixed the
Bottom regression mid-iteration.

## Build cost

Cold debug build (iteration 0001): ~100s.
Incremental build after this entire refactor: 2.5s.

The fear that the wider geometry refactor would trigger a
multi-minute recompile didn't materialize. Everything stayed inside
`wezterm-gui` proper.

## Known polish gaps

- Active-tab background extends only as wide as the title text, not the full bar width.
- `+` new-tab button styling doesn't match the tab visual style.
- No close (x) button per tab in vertical mode.
- Right-status, left-status, window-buttons not yet plumbed into vertical layout.
- `tab_bar_pixel_width` is a fixed 220.0; should become a config knob and/or derive from longest title.
- Click hit-testing on a vertical tab is theoretically wired (each tab carries `UIItemType::TabBar`), but hasn't been exercised. The mouse-event refactor uses the same insets so coordinates should already land correctly.
- Non-fancy tab bar in Left mode quietly falls back to Top; would be cleaner to log a one-time warning at config load.
- Resize behavior with a vertical strip hasn't been smoke-tested. The pixel_width math now adds the strip width, so the window's initial size grows by 220px when Left is set. Open question: do we want that, or should the strip eat into the cell area instead?

## Verdict

Vertical tab support is real, end-to-end, in this binary. Calling
the long-horizon goal hit: the architecture admits a third position
cleanly, the box model accommodates vertical stacking via the
existing Block primitive, and no horizontal regression slipped in.

Remaining work is polish and config surface, not architecture.

## Next iteration plan

If continuing:

1. Make active-tab and new-tab elements fill the bar's full width (probably a `min_width(tab_bar_pixel_width)` on each child plus an outer padding tweak).
2. Add the close (x) per-tab button in vertical mode.
3. Promote `tab_bar_pixel_width` to a config field (`tab_bar_pixel_width: Dimension`).
4. Plumb left/right status into the vertical layout (top of strip and bottom of strip respectively).
5. Resize smoke test: shrink and grow window, confirm cell area and bar both behave.
6. Document the new field in `docs/config/lua/config/tab_bar_position.md`.
