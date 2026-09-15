---
tags:
  - mouse
---
# `precise_scroll_scale = 15.0`

{{since('nightly', outline=true)}}

Some devices, such as the Apple Magic Mouse and trackpads, provide *precise*
scrolling deltas measured in pixels. Since wezterm doesn’t know the exact pixel
size of a terminal cell, it applies a scale factor to approximate a reasonable
scroll speed.

The `precise_scroll_scale` option controls this scale factor. Smaller values
make scrolling faster and more sensitive, while larger values slow it down.

The default value is `15.0`, which is tuned for typical font sizes and DPI
settings.
