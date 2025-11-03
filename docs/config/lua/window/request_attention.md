# window:request_attention(enabled)

{{since('nightly')}}

Requests (or clears) user attention for the window in a platform-specific manner.

When `enabled` is `true`, the window manager is asked to indicate that the window
needs attention (e.g., by flashing the taskbar entry). When `enabled` is `false`,
the attention request is cleared.

The attention hint is automatically cleared when the window regains focus.

|OS             |Behavior|
|---------------|--------|
|X11            |Sets `_NET_WM_STATE_DEMANDS_ATTENTION`, typically causes window to flash or highlight in taskbar|
|Wayland        |Uses XDG activation protocol; compositor decides how to show urgency (varies by compositor)|
|macOS          |Bounces the dock icon once using `NSInformationalRequest`|
|Windows        |Flashes the taskbar button orange until window gains focus|

This method can be used to implement custom attention/notification behavior,
complementing the automatic [bell_urgency_hint](../config/bell_urgency_hint.md)
configuration option.

```lua
wezterm.on('bell', function(window, pane)
  -- Request attention when any pane rings the bell
  window:request_attention(true)
end)
```

```lua
wezterm.on('update-status', function(window, pane)
  local mux_window = window:mux_window()
  local active_tab = mux_window:active_tab()

  -- Request attention if there are background tabs with new output
  for _, tab in ipairs(mux_window:tabs()) do
    if tab:tab_id() ~= active_tab:tab_id() then
      if tab:get_metadata().has_unseen_output then
        window:request_attention(true)
        return
      end
    end
  end

  window:request_attention(false)
end)
```

See also [bell_urgency_hint](../config/bell_urgency_hint.md), [bell event](../window-events/bell.md).
