# `window:set_font_scale(scale)`

{{since('nightly')}}

Zooms the window to the font scale `scale`, a positive number, as
[IncreaseFontSize](../keyassignment/IncreaseFontSize.md) and
[DecreaseFontSize](../keyassignment/DecreaseFontSize.md) do step by step:
`2.0` draws the font at twice the configured `font_size`, and `1.0` is what
[ResetFontSize](../keyassignment/ResetFontSize.md) returns to. Like them, it
keeps the window's rows and columns, resizing the window, unless
`adjust_window_size_when_changing_font_size` is `false`.

Together with [window:get_font_scale()](get_font_scale.md), it lets a
configuration give a new window the zoom an earlier one had.
