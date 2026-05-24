---
tags:
  - appearance
  - tab_bar
---

# `tab_bar_width = "Content"`

{{since('nightly')}}

Controls how tab widths are distributed across the tab bar.

Accepted values:

* `"Content"` (default) — each tab is sized to fit its title, leaving
  empty space after the last tab when the combined tab widths do not
  fill the bar. This preserves the historical behavior.
* `"Stretch"` — tabs share the bar width equally so the bar is fully
  covered, eliminating the empty trailing space that otherwise appears
  after the new-tab button.

```lua
config.tab_bar_width = "Stretch"
```

See also [`tab_max_width`](tab_max_width.md).
