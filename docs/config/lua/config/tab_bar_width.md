---
tags:
  - appearance
  - tab_bar
---

# `tab_bar_width = "Content"`

{{since('nightly')}}

Controls how tab widths are distributed across the **fancy** tab bar.
The classic (non-fancy) tab bar is not affected for now.

Accepted values:

* `"Content"` (default) — each tab is sized to fit its title, leaving
  empty space after the last tab when the combined tab widths do not
  fill the bar. This preserves the historical behavior.
* `"Stretch"` — tabs share the bar width equally so the bar is fully
  covered, eliminating the empty trailing space that otherwise appears
  after the new-tab button. Each tab's rendered width is forced to
  `(pixel_width − new_tab_button_width) / N_tabs`. The glyph-based
  [`tab_max_width`](tab_max_width.md) limit still caps the title
  string upstream, but in Stretch mode the pixel slot is typically
  the tighter constraint and titles longer than the slot are
  truncated with the standard `…` ellipsis.

```lua
config.tab_bar_width = "Stretch"
```

See also [`tab_max_width`](tab_max_width.md).
