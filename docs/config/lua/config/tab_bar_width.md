---
tags:
  - tab_bar
---
# `tab_bar_width = "200px"`

Controls the width of the tab bar when [tab_bar_position](tab_bar_position.md)
is set to `"Left"` or `"Right"`.

The value accepts pixel units (e.g., `"200px"`) and defaults to `"200px"`.

This option has no effect when the tab bar is in horizontal mode
(`"Top"` or `"Bottom"`).

```lua
local wezterm = require 'wezterm'

local config = wezterm.config_builder()

config.tab_bar_position = 'Left'
config.tab_bar_width = '250px'

return config
```
