# CopyMode `MoveForwardBigWord`

{{since('nightly')}}

Moves the CopyMode cursor position one whitespace-delimited WORD to the right.

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

return {
  key_tables = {
    copy_mode = {
      { key = 'W', mods = 'NONE', action = act.CopyMode 'MoveForwardBigWord' },
    },
  },
}
```
