# 0001 - Layout map and design constraint

Date: 2026-05-15

## What happened this iteration

- Confirmed toolchain: `cargo 1.95.0`, `rustc 1.95.0` (Homebrew).
- Cut feature branch `left-tabs` off `main` (last upstream commit `577474d`).
- Kicked off cold `cargo build -p wezterm-gui` in background (task `bw4kjm6wx`). Build still running at time of writing.
- Mapped every site in `wezterm-gui/src/` that touches tab-bar geometry. See "Layout map" below.

## Layout map

Single config knob today: `config.tab_bar_at_bottom: bool` (default false = top). Geometry assumes the bar is a horizontal strip occupying full window width, with height `tab_bar_pixel_height()` and y-position derived from `tab_bar_at_bottom`.

Touch points (file - role):

- `config/src/config.rs:483` - `tab_bar_at_bottom` schema.
- `wezterm-gui/src/termwindow/render/tab_bar.rs` - core paint entry `paint_tab_bar`, computes `tab_bar_y`, calls `render_screen_line` for non-fancy or dispatches to `paint_fancy_tab_bar`. Also owns `tab_bar_pixel_height_impl`.
- `wezterm-gui/src/termwindow/render/fancy_tab_bar.rs` - box-model fancy bar build + paint. Already branches on `tab_bar_at_bottom` at line 450 for top vs bottom placement.
- `wezterm-gui/src/termwindow/render/pane.rs:65-74, 591-599` - shrinks the pane area by `(top_bar_height, bottom_bar_height)`.
- `wezterm-gui/src/termwindow/render/split.rs:20` - split-divider rendering offsets by tab bar height when at top.
- `wezterm-gui/src/termwindow/render/mod.rs:379` - generic offset by `tab_bar_pixel_height`.
- `wezterm-gui/src/termwindow/mouseevent.rs:72, 298-306` - hit-test: `first_line_offset` for cell coords, plus `(top_bar_height, bottom_bar_height)` for routing clicks to the bar vs the pane.
- `wezterm-gui/src/termwindow/paneselect.rs:64`, `charselect.rs:395`, `palette.rs:263` - modal overlays offset their top by the tab-bar height when at top.
- `wezterm-gui/src/termwindow/mod.rs:607, 658, 871, 1973-1986, 2116-2129` - window construction and resize-time geometry. `tab_bar_y` and `hovering_in_tab_bar` both live here.
- `wezterm-gui/src/termwindow/resize.rs:166-227, 260-285, 492-519` - all the rows/cols-from-pixels math adds/subtracts `tab_bar_height`.
- `wezterm-gui/src/resize_increment_calculator.rs:12, 26` - `tab_bar_height` field, added into vertical resize increment.
- `wezterm-gui/src/tabbar.rs:428` - `compute_ui_items` adjusts hover ordering depending on top vs bottom.

Every one of these treats the bar as horizontal. Going vertical means each one needs a `width` analogue alongside `height`, and pane area must shrink horizontally not vertically.

## Critical constraint surfaced

The non-fancy tab bar (`use_fancy_tab_bar = false`) renders the entire bar as a single horizontal terminal line via `render_screen_line` (`render/tab_bar.rs:54-98`). One line. Horizontally. That code path cannot stack tab titles vertically without a fundamental rewrite of how it shapes glyphs.

The fancy tab bar (`use_fancy_tab_bar = true`, the default) is box-model. Each tab is its own `ComputedElement` laid out by the `box_model` crate. That layout engine is general enough to stack children vertically (it already does column layouts for other UI).

Conclusion: **vertical tabs are only feasible via the fancy tab bar path.** Two options for the legacy path:

1. Document `tab_bar_position = Left|Right` as requiring `use_fancy_tab_bar = true`, log a warning and fall back to `Top` otherwise.
2. Drop the legacy path entirely when position is Left/Right, treating fancy as implicit in that case.

Picking option 1 for the first iteration. Less surprising for users who have legacy bar configs, easy to revisit.

## Proposed design

Add to `config/src/config.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromDynamic, ToDynamic)]
pub enum TabBarPosition {
    Top,
    Bottom,
    Left,
    // Right deferred until Left works
}

impl Default for TabBarPosition { fn default() -> Self { Self::Top } }

#[dynamic(default)]
pub tab_bar_position: TabBarPosition,
```

Keep `tab_bar_at_bottom` for back-compat. Resolution rule: if `tab_bar_position` is non-default, it wins. Otherwise derive from `tab_bar_at_bottom`. Document the new field, deprecate the old one in the changelog without removing it.

Then introduce a helper on `TermWindow`:

```rust
struct TabBarLayout {
    top: f32, bottom: f32, left: f32, right: f32, // pixel insets into the window
}
fn tab_bar_layout(&self) -> TabBarLayout { ... }
```

Refactor every `tab_bar_at_bottom` branch above to consume `TabBarLayout` instead. That's the wide-cut refactor that lights up Left support uniformly. Hit-testing, pane offsetting, split offsetting, modal offsetting all read from the same struct.

Render side: introduce `paint_fancy_tab_bar_vertical()` mirroring the horizontal one but stacking tab elements top-to-bottom. Width comes from a new config knob `tab_bar_pixel_width` (or derived from longest tab title, with a `tab_bar_min_width` clamp).

## Next iteration plan

1. Wait for cold build to finish. Note time. Confirm we have a runnable binary.
2. Write the screenshot driver script in `left-tabs/scripts/` (osascript window lookup + `screencapture -l`).
3. Launch the binary with a stock config, capture baseline screenshot, sanity-check the loop.
4. Make the smallest possible config-only change: add `TabBarPosition` enum and `tab_bar_position` field, leave runtime behavior identical. Build. Confirm still launches.
5. Then start the `TabBarLayout` refactor on top of that.

## Open questions for Kai (not blocking)

- Width policy for the vertical bar: fixed pixels, derived-from-content, or both with a min?
- When `tab_bar_position = Left` and `use_fancy_tab_bar = false`: warn + fall back (current plan), or hard error?

## Risk update

- Refactor scope estimate is now grounded: roughly 12 touch points need rewiring through `TabBarLayout`. Tractable, not a balloon.
- Build wall-time will be the dominant cost. First incremental build after the config-only change should tell us a lot.
