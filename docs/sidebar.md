The workspace sidebar is a toggleable panel that displays a tree view of all
workspaces and their tabs, giving you a visual overview of your terminal
session and quick access to workspace management.

```
+----------------------------------------------+-------------------+
|                                              | WORKSPACES        |
|                                              |▼ default    + ✕   |
|  Terminal content area                       |  ● 0: ~        ✕  |
|                                              |    1: nvim     ✕  |
|                                              |► build             |
|                                              |► deploy            |
|                                              |                    |
|                                              | + New Workspace    |
+----------------------------------------------+-------------------+
  ^                                              ^
  terminal panes                                 sidebar
```

### Toggling the Sidebar

The sidebar is toggled with <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>E</kbd>
by default. You can rebind this using the
[ToggleSidebar](config/lua/keyassignment/ToggleSidebar.md) key assignment:

```lua
local wezterm = require 'wezterm'

config.keys = {
  {
    key = 'b',
    mods = 'SHIFT|CTRL',
    action = wezterm.action.ToggleSidebar,
  },
}
```

### Workspace Operations

The sidebar lists all workspaces. Each workspace can be expanded or collapsed
to show or hide its tabs.

| Action | How |
|--------|-----|
| Switch workspace | Click the workspace name |
| Expand / collapse tab list | Click the `▶` / `▼` disclosure triangle |
| Rename workspace | Double-click the workspace name |
| Create new tab in workspace | Hover over the workspace row and click the `+` button |
| Close workspace | Hover over the workspace row and click the `✕` button |
| Create new workspace | Click `+ New Workspace` at the bottom of the sidebar |

The `+` and `✕` buttons are hidden by default and appear when you hover over
the workspace row. The `✕` button turns red on hover as a safety indicator.

### Tab Operations

When a workspace is expanded, its tabs are listed beneath it. The active tab
is marked with a green `●` indicator.

| Action | How |
|--------|-----|
| Switch to tab | Click the tab name |
| Rename tab | Double-click the tab name |
| Close tab | Hover over the tab row and click the `✕` button |

### Inline Rename

Double-clicking a workspace name or tab title enters inline rename mode. A
cursor appears at the end of the current name. The following keys are
available during rename:

| Key | Action |
|-----|--------|
| <kbd>Enter</kbd> | Confirm the new name |
| <kbd>Escape</kbd> | Cancel and revert to the original name |
| <kbd>Backspace</kbd> | Delete the previous character |
| Click elsewhere | Confirm the new name |

All other printable keys are inserted into the name.

### Configuration

The sidebar appearance and behavior can be configured through the following
options:

```lua
-- Width of the sidebar in cell columns (default: 40)
config.sidebar_width = 40

-- Which side of the window: "Left" or "Right" (default: "Right")
config.sidebar_position = "Right"

-- Whether the sidebar is open when a new window is created (default: false)
config.sidebar_default_visible = false
```

The sidebar uses your existing theme colors from
[window_frame](config/lua/config/window_frame.md) and
[tab_bar](config/lua/config/colors.md) configuration for a consistent look.

See the individual config docs for details:

* [sidebar_width](config/lua/config/sidebar_width.md)
* [sidebar_position](config/lua/config/sidebar_position.md)
* [sidebar_default_visible](config/lua/config/sidebar_default_visible.md)
