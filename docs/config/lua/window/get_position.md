# `window:get_position()`

{{since('nightly')}}

Returns where the window is on the screen, as a Lua table with the fields `x`
and `y`: the top left pixel of its content area, in the coordinates that
[window:set_position()](set_position.md) takes. Moving a window to the
position it reports leaves it where it is, so a configuration can record a
window's position and put a new window there later.

Wayland does not tell applications where their windows are, so on Wayland
this returns `nil`.

```lua
local position = window:get_position()
if position then
  wezterm.log_info(string.format('at %d, %d', position.x, position.y))
end
```
