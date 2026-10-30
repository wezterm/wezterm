# `window:set_inner_size(width, height)`

{{since('20220807-113146-c2fee766')}}

Resizes the inner portion of the window (excluding any window decorations) to
the specified width and height, in pixels at the scale of the screen the window
is on.

{{since('nightly', inline=True)}} The window is no larger than the visible area
of its screen, as [window:set_content_size()](set_content_size.md) describes.
A size in pixels means a different window size on screens of different scales;
[window:get_content_size()](get_content_size.md) and `set_content_size` work in
units that mean the same on every screen.
