# `ToggleSidebar`

Toggles the workspace sidebar panel for the current window.

The sidebar displays a tree view of all workspaces and their tabs,
allowing you to switch workspaces, rename them, create new workspaces,
and close existing ones.

```lua
local wezterm = require 'wezterm'

config.keys = {
  {
    key = 'e',
    mods = 'SHIFT|CTRL',
    action = wezterm.action.ToggleSidebar,
  },
}
```

See also: [sidebar_width](../config/sidebar_width.md),
[sidebar_position](../config/sidebar_position.md),
[sidebar_default_visible](../config/sidebar_default_visible.md),
and the [Sidebar Guide](../../../sidebar.md).
