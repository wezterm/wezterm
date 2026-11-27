# CopyMode `MoveForwardBigWordEnd`

{{since('nightly')}}

Moves the CopyMode cursor position forward to the end of a whitespace-delimited WORD.

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

return {
  key_tables = {
    copy_mode = {
      {
        key = 'E',
        mods = 'NONE',
        action = act.CopyMode 'MoveForwardBigWordEnd',
      },
    },
  },
}
```
