---
tags:
  - appearance
---
# `dim_tint_factor = 1.0`

{{since('nightly')}}

Controls the foreground brightness of text with the dim/faint attribute (SGR 2).
The value must be between `0.0` and `1.0` and multiplies the RGB components in
linear-light space, preserving alpha. The default, `1.0`, leaves colors unchanged;
`0.5` halves linear-light brightness, and `0.0` makes the foreground black.

This is applied in addition to any font selection configured through
[`font_rules`](font_rules.md), before the
[`foreground_text_hsb`](foreground_text_hsb.md) transform.

```lua
config.dim_tint_factor = 0.5
```
