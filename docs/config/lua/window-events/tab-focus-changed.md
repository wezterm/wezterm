# `tab-focus-changed`

{{since('nightly')}}

The `tab-focus-changed` event is emitted when switching to a different tab
within a window.

This event is fire-and-forget from the perspective of wezterm; it fires the
event to advise of the tab change, but has no other expectations.

The first event parameter is a [`window` object](../window/index.md) that
represents the gui window.

The second event parameter is a [`pane` object](../pane/index.md) that
represents the active pane in the newly focused tab.

```lua
local wezterm = require 'wezterm'

wezterm.on('tab-focus-changed', function(window, pane)
  wezterm.log_info(
    'switched to tab containing pane ',
    pane:pane_id(),
    ' in window ',
    window:window_id()
  )
end)
```

See also [window-focus-changed](window-focus-changed.md) which is emitted when
the window itself gains or loses focus.
