# `window:get_content_size()`

{{since('nightly')}}

Returns the size of the window's content area (the window less its
decorations) as a Lua table with the fields `width` and `height`, in the units
the windowing system keeps a window's size in while the window moves between
screens of different scales:

- on macOS, points: a window 800 points wide is 1600 pixels wide on a Retina
  display and 800 on a display of scale 1;
- on Wayland, the surface's logical pixels;
- on X11 and Windows, pixels, which a window keeps when it moves to another
  screen.

Unlike the pixel sizes of [window:get_dimensions()](get_dimensions.md), the
size means the same on every screen, so a configuration can record a window's
size and give it to a new window with
[window:set_content_size()](set_content_size.md), whichever screen either is
on. The values may have a fractional part.

```lua
local size = window:get_content_size()
wezterm.log_info(string.format('%gx%g', size.width, size.height))
```
