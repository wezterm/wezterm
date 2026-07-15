---
tags:
  - tab_bar
---
# `tab_bar_position = "Top"`

Controls where the tab bar is rendered within the window.

Possible values are:

* `"Top"` - tab bar at the top of the window (default)
* `"Bottom"` - tab bar at the bottom of the window
* `"Left"` - tab bar on the left side of the window (vertical tabs)
* `"Right"` - tab bar on the right side of the window (vertical tabs)

When set to `"Left"` or `"Right"`, the [fancy tab
bar](use_fancy_tab_bar.md) is always used, regardless of the
`use_fancy_tab_bar` setting. The width of the vertical tab bar
can be controlled with [tab_bar_width](tab_bar_width.md).

This option takes precedence over the legacy
[tab_bar_at_bottom](tab_bar_at_bottom.md) option. If
`tab_bar_position` is left at its default value of `"Top"` and
`tab_bar_at_bottom = true`, the tab bar will be placed at the
bottom for backward compatibility.

```lua
local wezterm = require 'wezterm'

local config = wezterm.config_builder()

-- Place the tab bar on the left side with a 250px width
config.tab_bar_position = 'Left'
config.tab_bar_width = '250px'

return config
```
