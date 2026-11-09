# CopyMode `MoveBackwardWordStart`

{{since('next')}}

Moves the CopyMode cursor position backward to the start of the current or previous word, ignoring stop characters.

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

return {
  key_tables = {
    copy_mode = {
      {
        key = 'B',
        mods = 'SHIFT',
        action = act.CopyMode 'MoveBackwardWordStart',
      },
    },
  },
}
```
