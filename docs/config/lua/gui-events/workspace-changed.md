# `workspace-changed`

{{since('nightly')}}

The `workspace-changed` event is emitted when the active workspace changes,
including when the workspace in front is renamed and when wezterm moves you off
a workspace whose last window has closed.

The first parameter is the name of the workspace that is now active.
The second is the name of the workspace that was active before it.

It is emitted once for the change.
It does not fire for the workspace that is active at startup.

The workspace that was active before may no longer exist: a rename retires the
name it replaced, and a workspace whose last window has closed is gone by the
time the change it caused is reported.

The example below binds `ALT-l` to switching back to the workspace you came from.
It records each previous name as it is reported,
and checks that the name still exists before switching to it:

```lua
local wezterm = require 'wezterm'
local act = wezterm.action
local config = wezterm.config_builder()

wezterm.on('workspace-changed', function(workspace, prior)
  wezterm.GLOBAL.previous_workspace = prior
end)

config.keys = {
  {
    key = 'l',
    mods = 'ALT',
    action = wezterm.action_callback(function(window, pane)
      local previous = wezterm.GLOBAL.previous_workspace
      for _, name in ipairs(wezterm.mux.get_workspace_names()) do
        if name == previous then
          window:perform_action(
            act.SwitchToWorkspace { name = previous },
            pane
          )
          return
        end
      end
    end),
  },
}

return config
```

See also: [SwitchToWorkspace](../keyassignment/SwitchToWorkspace.md),
[wezterm.mux.get_active_workspace](../wezterm.mux/get_active_workspace.md),
[wezterm.mux.set_active_workspace](../wezterm.mux/set_active_workspace.md).
