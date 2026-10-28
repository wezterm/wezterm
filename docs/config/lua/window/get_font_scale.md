# `window:get_font_scale()`

{{since('nightly')}}

Returns the window's font scale, the factor its font is drawn at relative to
the configured `font_size`: `1.0` until the window is zoomed with
[IncreaseFontSize](../keyassignment/IncreaseFontSize.md),
[DecreaseFontSize](../keyassignment/DecreaseFontSize.md) or
[window:set_font_scale()](set_font_scale.md), and again after
[ResetFontSize](../keyassignment/ResetFontSize.md).
