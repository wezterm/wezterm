# `tab:move_to_window(target_window)`

{{since('nightly')}}

Moves the tab (including all its panes and split layout) from its current window
to the specified target window. The tab is appended to the end of the target window's
tab list.

Returns `true` if the tab moved to a different window and `false` if it was
already in the target window.

## Parameters

- `target_window` - A [MuxWindow](../mux-window/index.md) object representing the
  destination window

## Returns

Returns `true` if the tab moved, and `false` if it was already in the target
window. Raises a Lua error if the operation fails.

## Example

```lua
local wezterm = require 'wezterm'
local config = wezterm.config_builder()

config.keys = {
  {
    key = 'M',
    mods = 'LEADER|SHIFT',
    action = wezterm.action_callback(function(win, pane)
      local tab = pane:tab()
      local windows = wezterm.mux.all_windows()

      -- Move current tab to the next window
      if #windows >= 2 then
        local current_window = pane:window()
        for _, w in ipairs(windows) do
          if w:window_id() ~= current_window:window_id() then
            tab:move_to_window(w)
            break
          end
        end
      end
    end),
  },
}

return config
```

## Notes

- If the tab is already in the target window, this is a no-op
- The source window gets closed if it becomes empty after the move
- All panes and their split layout within the tab are preserved
- The target window's currently active tab is not changed

## See Also

- [tab:activate()](activate.md)
- [pane:move_to_new_tab()](../pane/move_to_new_tab.md)
- [pane:move_to_new_window()](../pane/move_to_new_window.md)
- [wezterm.mux.all_windows()](../mux/all_windows.md)
- [MuxWindow](../mux-window/index.md)
