# `SelectAll`

Selects all text in the active pane, including its scrollback and visible
viewport, without entering Copy Mode or Search Mode.

For example, bind it to `CMD-A` on macOS and then use the default `CMD-C`
binding to copy the selection:

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

config.keys = {
  {
    key = 'a',
    mods = 'SUPER',
    action = act.SelectAll,
  },
}
```

{{since('nightly')}}
