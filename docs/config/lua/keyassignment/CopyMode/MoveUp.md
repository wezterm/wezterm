# CopyMode `MoveUp`

{{since('20220624-141144-bd1b7c5d')}}

Moves the CopyMode cursor position one cell up.
In the default copy mode key table, a numeric prefix can be used with `k` to
move multiple rows, so `10k` moves up 10 rows.

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

return {
  key_tables = {
    copy_mode = {
      { key = 'UpArrow', mods = 'NONE', action = act.CopyMode 'MoveUp' },
    },
  },
}
```
