# `window:get_hyperlink_at_mouse_cursor()`

{{since('nightly')}}

Returns the URI of the hyperlink currently under the mouse cursor as a string,
or `nil` if the mouse is not over a hyperlink.

The returned URI is exactly what
[OpenLinkAtMouseCursor](../keyassignment/OpenLinkAtMouseCursor.md) would open,
and includes both OSC 8 hyperlinks emitted by the terminal application (see
[Hyperlinks](../../../hyperlinks.md)) and matches against your
[hyperlink_rules](../config/hyperlink_rules.md).

This lets a `mouse_bindings` callback branch on whether a link is present and
decide what to do with it, without relying on the (undocumented) side effect of
the `open-uri` event firing synchronously inside `OpenLinkAtMouseCursor`.

```lua
local wezterm = require 'wezterm'

config.mouse_bindings = {
  -- Ctrl-click: if a link is under the cursor, copy it to the clipboard
  -- instead of opening it. Otherwise, do nothing.
  {
    event = { Up = { streak = 1, button = 'Left' } },
    mods = 'CTRL',
    action = wezterm.action_callback(function(window, pane)
      local link = window:get_hyperlink_at_mouse_cursor()
      if link then
        window:copy_to_clipboard(link)
      end
    end),
  },
}
```

To run one action when a link is present and a different action when there is
none — without writing a callback — you can use the optional `fallback` of
[OpenLinkAtMouseCursor](../keyassignment/OpenLinkAtMouseCursor.md).

To find the *cell* under the cursor (for example to feed
[pane:get_semantic_zone_at(x, y)](../pane/get_semantic_zone_at.md)), see
[window:get_mouse_position()](get_mouse_position.md).
