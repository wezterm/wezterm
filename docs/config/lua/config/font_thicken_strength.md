---
tags:
  - font
---
# `font_thicken_strength = 255`

Controls the strength of [font_thicken](font_thicken.md).

Valid values are integers between `0` and `255`. `0` does not correspond to
no thickening; it selects the lightest available thickening.

This setting only has an effect when `font_thicken = true`, using the
`FreeType` rasterizer for outline glyphs.

```lua
config.font_thicken = true
config.font_thicken_strength = 128
```
