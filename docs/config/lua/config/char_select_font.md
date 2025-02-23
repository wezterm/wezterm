---
tags:
  - appearance
  - char_select
  - font
---
# `char_select_font`

{{since('nightly')}}

Configures the font to use for character selection. The `char_select_font`
setting can specify a set of fallbacks and other options, and is described
in more detail in the [Fonts](../../fonts.md) section.

If not specified, the font is same as the font in `window_frame.font`

You will typically use [wezterm.font](../wezterm/font.md) or
[wezterm.font_with_fallback](../wezterm/font_with_fallback.md) to specify the
font.
