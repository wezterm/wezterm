---
tags:
  - appearance
  - font
---
# `dim_opacity = 1.0`

{{since('nightly')}}

The opacity wezterm draws dim text at, between `0.0` and `1.0`.

```lua
config.dim_opacity = 0.6
```

The default value is `1.0` when [track_bold_and_dim_separately](track_bold_and_dim_separately.md)
is off, or `0.5` otherwise.

!!! note
    Setting this value below `1.0` disables default font rule that makes dim text
    render with thinner font.

!!! warning
    [text_min_contrast_ratio](text_min_contrast_ratio.md) doesn't consider
    text opacity. With `dim_opacity` set below `1.0`, so faded text can end up
    below the promised ratio.
