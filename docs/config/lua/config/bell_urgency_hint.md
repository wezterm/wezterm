---
tags:
  - bell
---
# `bell_urgency_hint`

{{since('nightly')}}

When the BEL ascii sequence is sent to a pane, the bell is "rung" in that pane.

You may choose to configure the `bell_urgency_hint` option to request user attention
from the window manager when the bell rings and the window is not focused.

The following are possible values:

* `"Disabled"` - don't set urgency hint. This is the default.
* `"Enabled"` - set the urgency hint when BEL is received and the window is not focused

When enabled, the behavior is platform-specific:

* **X11**: Sets the `_NET_WM_STATE_DEMANDS_ATTENTION` window state, which typically causes the window to flash or highlight in the taskbar/window list
* **Wayland**: Uses XDG activation protocol; compositor decides how to show urgency (varies by compositor)
* **macOS**: Bounces the dock icon once
* **Windows**: Flashes the taskbar button orange until window gains focus

The urgency hint is automatically cleared when the window regains focus.

```lua
config.bell_urgency_hint = "Enabled"
```

See also [audible_bell](audible_bell.md), [visual_bell](visual_bell.md), [bell event](../window-events/bell.md), and [window:request_attention()](../window/request_attention.md).
