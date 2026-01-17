# `pane-focus-changed`

{{since('nightly')}}

The `pane-focus-changed` event is emitted when a different pane becomes focused
within a window, whether by switching tabs or by switching panes within a tab.

This event is fire-and-forget from the perspective of wezterm; it fires the
event to advise of the focus change, but has no other expectations.

The first event parameter is a [`window` object](../window/index.md) that
represents the gui window.

The second event parameter is a [`pane` object](../pane/index.md) that
represents the newly focused pane.

```lua
local wezterm = require 'wezterm'

wezterm.on('pane-focus-changed', function(window, pane)
  wezterm.log_info(
    'pane ',
    pane:pane_id(),
    ' is now focused in window ',
    window:window_id()
  )
end)
```

See also [window-focus-changed](window-focus-changed.md) which is emitted when
the window itself gains or loses focus.
