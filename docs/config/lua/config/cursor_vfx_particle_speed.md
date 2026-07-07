---
tags:
  - appearance
  - text_cursor
---
# `cursor_vfx_particle_speed`

{{since('nightly')}}

Controls the initial speed of particles produced by
[cursor_trail_style](cursor_trail_style.md), in cells per second.

Higher values make particles fly further and faster from the cursor; lower
values keep the effect more contained. The default is `8.0`.

```lua
config.cursor_trail_style = 'Torpedo'
config.cursor_vfx_particle_speed = 8.0
```
