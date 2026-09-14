---
tags:
  - appearance
  - font
---
# `track_bold_and_dim_separately = false`

{{since('nightly')}}

When `true`, bold and dim are two independent facts about a cell and wezterm
reads them separately. When `false`, which is the default, wezterm uses
whichever of the two arrived most recently.

```lua
config.track_bold_and_dim_separately = true
```

!!! note
    When this option is enabled, the default value for [dim_opacity](dim_opacity.md)
    becomes `0.5`, which in turn disables the default font rule that makes dim text
    render with a thinner font. Setting `dim_opacity = 1.0` alongside this option
    brings that rule back.

!!! warning
    When this option is enabled, [font_rules](font_rules.md) can't use `intensity`
    matcher. You will need to adjust such rules to use `bold` and `dim` matchers.
