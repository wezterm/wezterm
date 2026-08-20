# `window:get_mouse_position()`

{{since('nightly')}}

Returns the terminal cell currently under the mouse cursor, or `nil` if the
mouse is not over a pane (for example, before any mouse event has been
processed in this window).

The result is a table with two fields:

| Field | Type | Meaning                                                                    |
| ----- | ---- | -------------------------------------------------------------------------- |
| `x`   | int  | The cell column, where `0` is the left-most column.                        |
| `y`   | int  | The [stable row index](../pane/get_semantic_zone_at.md) of the hovered row. |

The `x`/`y` values use the same coordinate convention as
[pane:get_semantic_zone_at(x, y)](../pane/get_semantic_zone_at.md), so the two
compose directly.

The returned position reflects the most recently processed mouse event in this
window; it is updated on every mouse move, press, release and wheel event. In a
`mouse_bindings` callback it is therefore the cell that was just clicked or
hovered.

```lua
local wezterm = require 'wezterm'

config.mouse_bindings = {
  -- Ctrl+Right click: open the semantic zone (e.g. a command's output)
  -- under the click in `$EDITOR`, like kitty's mouse_show_command_output.
  {
    event = { Down = { streak = 1, button = 'Right' } },
    mods = 'CTRL',
    action = wezterm.action_callback(function(window, pane)
      local pos = window:get_mouse_position()
      if not pos then
        return
      end
      local zone = pane:get_semantic_zone_at(pos.x, pos.y)
      if zone then
        local text = pane:get_text_from_semantic_zone(zone)
        wezterm.log_info('zone under cursor: ' .. text)
        -- ...open `text` in your `$EDITOR` here...
      end
    end),
  },
}
```

To find out whether a hyperlink is under the cursor, see
[window:get_hyperlink_at_mouse_cursor()](get_hyperlink_at_mouse_cursor.md).
