# `window:set_content_size(width, height)`

{{since('nightly')}}

Resizes the window's content area to `width` by `height` in the units that
[window:get_content_size()](get_content_size.md) reports, which mean the same
on every screen. The size is converted for the screen the window is on when the
resize applies: after a [window:set_position()](set_position.md) asked for
before it, that is the screen the window was moved to.

The top left of the content area stays where it is, so a window resized after
being moved with `set_position` stays at that position.

As with [window:set_inner_size()](set_inner_size.md), the window is no larger
than the visible area of its screen: the screen less the menu bar and the Dock
on macOS, less the taskbar on Windows, and the bounds the compositor suggests on
Wayland (a compositor that suggests none leaves the size as asked). X11 tells a
window nothing of the panels on a screen, so there the limit is the whole
screen.

```lua
window:set_position(100, 120)
window:set_content_size(1200, 800)
```
