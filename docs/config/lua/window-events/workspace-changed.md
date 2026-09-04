# `workspace-changed`

{{since('nightly')}}

The `workspace-changed` event is emitted when the active workspace changes.

It fires for every kind of switch, whether it came from a key assignment such
as [SwitchToWorkspace](../keyassignment/SwitchToWorkspace.md), from
[wezterm.mux.set_active_workspace](../wezterm.mux/set_active_workspace.md), from
renaming the workspace that is in front, or from wezterm itself moving you off
a workspace whose last window has just been closed.

This event is fire-and-forget from the perspective of wezterm; it fires the
event to advise of the change, but has no other expectations.

The first event parameter is a [`window` object](../window/index.md) that
represents the gui window.

The second event parameter is a [`pane` object](../pane/index.md) that
represents the active pane in that window.

The third parameter is the name of the workspace that is now active. It is the
same value that [window:active_workspace()](../window/active_workspace.md)
returns.

The fourth parameter is the name of the workspace that was active before the
change, or `nil` if there wasn't one. This is the only place that name is
available, as the mux has already replaced it by the time the event is
delivered.

Like [window-config-reloaded](window-config-reloaded.md), this event is emitted
once per gui window. It is emitted before those windows have been reconciled
against the new workspace, so the window passed to the callback may be one that
is about to be repurposed for a different mux window, or closed.

```lua
local wezterm = require 'wezterm'

wezterm.on('workspace-changed', function(window, pane, workspace, prior)
  wezterm.log_info(
    'workspace is now ' .. workspace .. ', was ' .. tostring(prior)
  )
end)
```

The `prior` parameter makes it possible to build a "switch back to where I just
was" key assignment, in the spirit of
[ActivateLastTab](../keyassignment/ActivateLastTab.md):

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

wezterm.on('workspace-changed', function(window, pane, workspace, prior)
  if prior then
    -- Note that only scalars survive a round trip through wezterm.GLOBAL
    wezterm.GLOBAL.previous_workspace = prior
  end
end)

config.keys = {
  {
    key = 'l',
    mods = 'ALT',
    action = wezterm.action_callback(function(window, pane)
      local previous = wezterm.GLOBAL.previous_workspace
      if previous then
        window:perform_action(
          act.SwitchToWorkspace { name = previous },
          pane
        )
      end
    end),
  },
}
```
