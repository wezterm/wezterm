---
tags:
  - search
---

# CopyMode `SearchBackwardRelativeToCursor`

{{since('nightly')}}

Put CopyMode into editing mode with the match before the current cursor
position being activated if present: keyboard input will be directed to
the search pattern editor.

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

return {
  key_tables = {
    copy_mode = {
      -- This action is not bound by default in wezterm
      {
        key = '?',
        mods = 'SHIFT',
        action = act.CopyMode 'SearchBackwardRelativeToCursor',
      },
    },
  },
}
```
