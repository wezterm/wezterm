# `window:set_config_overrides(overrides)`

{{since('20210314-114017-04b7cedd')}}

Changes the set of configuration overrides for the window.
The config file is re-evaluated and any CLI overrides are
applied, followed by the keys and values from the `overrides`
parameter.

This can be used to override configuration on a per-window basis;
this is only useful for options that apply to the GUI window, such
as rendering the GUI.

Each call to `window:set_config_overrides` will emit the
[window-config-reloaded](../window-events/window-config-reloaded.md) event for
the window.  If you are calling this method from inside the handler
for `window-config-reloaded` you should take care to only call `window:set_config_overrides`
if the actual override values have changed to avoid a loop.

In this example, a key assignment (`CTRL-SHIFT-E`) is used to toggle the use of
ligatures in the current window:

```lua
local wezterm = require 'wezterm'

wezterm.on('toggle-ligature', function(window, pane)
  local overrides = window:get_config_overrides() or {}
  if not overrides.harfbuzz_features then
    -- If we haven't overridden it yet, then override with ligatures disabled
    overrides.harfbuzz_features = { 'calt=0', 'clig=0', 'liga=0' }
  else
    -- else we did already, and we should disable out override now
    overrides.harfbuzz_features = nil
  end
  window:set_config_overrides(overrides)
end)

return {
  keys = {
    {
      key = 'E',
      mods = 'CTRL',
      action = wezterm.action.EmitEvent 'toggle-ligature',
    },
  },
}
```

In this example, a key assignment (`CTRL-SHIFT-B`) is used to toggle opacity
for the window:

```lua
local wezterm = require 'wezterm'

wezterm.on('toggle-opacity', function(window, pane)
  local overrides = window:get_config_overrides() or {}
  if not overrides.window_background_opacity then
    overrides.window_background_opacity = 0.5
  else
    overrides.window_background_opacity = nil
  end
  window:set_config_overrides(overrides)
end)

return {
  keys = {
    {
      key = 'B',
      mods = 'CTRL',
      action = wezterm.action.EmitEvent 'toggle-opacity',
    },
  },
}
```

In this example, two key assignments (`CTRL-SHIFT-PageUp` and `CTRL-SHIFT-PageDown`)
adjust the window opacity incrementally instead of toggling. Each press reads
the current opacity (falling back to the configured value if no override is
set yet), nudges it by 0.1, and clamps the result to a sensible range so the
window stays visible:

```lua
local wezterm = require 'wezterm'

local function adjust_opacity(window, delta)
  local overrides = window:get_config_overrides() or {}
  -- Pick up the current opacity from the override, then the static config,
  -- defaulting to fully opaque if neither is set.
  local current = overrides.window_background_opacity
    or window:effective_config().window_background_opacity
    or 1.0
  -- Clamp to [0.1, 1.0] so the window doesn't disappear or exceed full opacity.
  overrides.window_background_opacity =
    math.max(0.1, math.min(1.0, current + delta))
  window:set_config_overrides(overrides)
end

wezterm.on('increase-opacity', function(window, _)
  adjust_opacity(window, 0.1)
end)

wezterm.on('decrease-opacity', function(window, _)
  adjust_opacity(window, -0.1)
end)

return {
  keys = {
    {
      key = 'PageUp',
      mods = 'CTRL|SHIFT',
      action = wezterm.action.EmitEvent 'increase-opacity',
    },
    {
      key = 'PageDown',
      mods = 'CTRL|SHIFT',
      action = wezterm.action.EmitEvent 'decrease-opacity',
    },
  },
}
```

