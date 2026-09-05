---
tags:
  - appearance
---
# `pane_padding`

Controls the amount of padding reserved as a gutter around a pane, on
whichever side(s) it borders a sibling pane in a split.

Unlike [window_padding](window_padding.md), which pads between the window
border and the terminal cells, `pane_padding` widens the gap *between*
panes. The divider line that wezterm draws between split panes keeps its
usual width and is centered within the gutter.

Padding is measured using the same units as `window_padding`: `"1px"`,
`"1pt"`, `"1cell"` or `"1%"`, or a plain number of pixels.

```lua
config.pane_padding = {
  left = 0,
  right = 0,
  top = 0,
  bottom = 0,
}
```

The default is `0` on every edge, so existing configs see no change in
split layout until `pane_padding` is explicitly set.

Only the side(s) of a pane that actually border a sibling are affected;
`left`/`top`/`right`/`bottom` describe the edge of the *pane*, not the
direction of the split, so a pane's `pane_padding` only ever has a visual
effect on the edge(s) where it touches another pane.

{{since('nightly')}}
