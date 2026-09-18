# `ShowContextMenu`

{{since('nightly')}}

Pops up a context menu at the current mouse position, offering Copy and
Paste actions for the current pane.

The Copy entry is disabled when there is no selection, and the Paste
entry is disabled when the clipboard doesn't contain text. The menu
follows the OS light/dark appearance.

Currently only the Windows backend shows a menu; the action is ignored
on other platforms.

This example shows how to bind the right mouse button to show the menu:

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

config.mouse_bindings = {
  {
    event = { Down = { streak = 1, button = 'Right' } },
    mods = 'NONE',
    action = act.ShowContextMenu,
  },
}
```
