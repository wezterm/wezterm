# CopyMode `MoveForwardParagraph`

{{since('nightly')}}

Moves the CopyMode cursor forward to the next paragraph boundary (the blank line
following the current or next paragraph, or the bottom of scrollback if none is
found), skipping consecutive blank lines.

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

return {
  key_tables = {
    copy_mode = {
      {
        key = '}',
        mods = 'SHIFT',
        action = act.CopyMode 'MoveForwardParagraph',
      },
    },
  },
}
```
