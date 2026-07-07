---
tags:
  - appearance
  - text_cursor
---
# `cursor_vfx_opacity`

{{since('nightly')}}

Controls the opacity of the particles and highlights produced by
[cursor_trail_style](cursor_trail_style.md).

* `1.0` — fully opaque.
* `0.0` — invisible.

The default is `0.6`.

```lua
config.cursor_trail_style = 'Torpedo'
config.cursor_vfx_opacity = 0.6
```
