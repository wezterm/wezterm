# `OpenLinkAtMouseCursor`

If the current mouse cursor position is over a cell that contains
a hyperlink, this action causes that link to be opened.

{{since('nightly')}}
You may set an optional `fallback` action that is performed when the mouse
is *not* over a hyperlink. This lets a single chord "open whatever's here":
open the link if one is present, otherwise run the fallback (for example,
resolve the clicked word as a path or issue reference and open that).

```lua
config.mouse_bindings = {
  -- Ctrl-click will open the link under the mouse cursor
  {
    event = { Up = { streak = 1, button = 'Left' } },
    mods = 'CTRL',
    action = wezterm.action.OpenLinkAtMouseCursor,
  },
}
```

With a fallback:

```lua
local wezterm = require 'wezterm'

config.mouse_bindings = {
  -- Ctrl-click: open the hyperlink under the cursor if there is one,
  -- otherwise copy the clicked word (e.g. a path or issue ref) so you
  -- can act on it. Replace the fallback with whatever suits your flow.
  {
    event = { Up = { streak = 1, button = 'Left' } },
    mods = 'CTRL',
    action = wezterm.action.OpenLinkAtMouseCursor {
      fallback = wezterm.action_callback(function(window, pane)
        local link = window:get_hyperlink_at_mouse_cursor()
        if not link then
          -- No link: fall back to your own logic here.
          wezterm.log_info('no link under cursor')
        end
      end),
    },
  },
}
```

If you need to inspect the link (rather than open it), or branch on link
presence directly in Lua, see
[window:get_hyperlink_at_mouse_cursor()](../window/get_hyperlink_at_mouse_cursor.md).
