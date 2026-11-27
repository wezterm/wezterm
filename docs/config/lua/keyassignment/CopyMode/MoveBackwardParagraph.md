# CopyMode `MoveBackwardParagraph`

{{since('nightly')}}

Moves the CopyMode cursor backward to the start of the previous paragraph
(the blank line preceding the current or previous paragraph, or the top of
scrollback if none is found), skipping consecutive blank lines.

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

return {
  key_tables = {
    copy_mode = {
      {
        key = '{',
        mods = 'SHIFT',
        action = act.CopyMode 'MoveBackwardParagraph',
      },
    },
  },
}
```
