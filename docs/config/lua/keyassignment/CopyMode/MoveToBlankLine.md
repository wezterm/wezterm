# CopyMode `MoveToBlankLine`

Moves the CopyMode cursor position to the next blank line in the specified
direction, or to the end of scrollback if no blank line is found.

This is similar to `tmux`'s `previous-paragraph` and `next-paragraph` bindings.

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

return {
  key_tables = {
    copy_mode = {
      {
        key = "{",
        action = act.CopyMode { MoveToBlankLine = "Up" },
      },
      {
        key = "}",
        action = act.CopyMode { MoveToBlankLine = "Down" },
      },
    },
  },
}
```

