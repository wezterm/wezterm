# CopyMode `JumpToMatchingBracket`

{{since('nightly')}}

Moves the CopyMode cursor position to the position of the matching bracket
if found. This applies to the bracket pairs `{}`, `[]`, `()`

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

return {
  key_tables = {
    copy_mode = {
      {
        key = '%',
        mods = 'NONE',
        action = act.CopyMode 'JumpToMatchingBracket',
      },
    },
  },
}
```


