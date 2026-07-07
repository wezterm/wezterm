---
tags:
  - appearance
  - text_cursor
---
# `cursor_vfx_particle_density`

{{since('nightly')}}

Controls how many particles are spawned per cell of cursor movement for
[cursor_trail_style](cursor_trail_style.md) particle effects.

Higher values produce a denser trail; lower values are more sparse. The
default is `0.7`.

A hard cap of 256 simultaneous live particles per pane is always enforced
regardless of this value.

```lua
config.cursor_trail_style = 'Torpedo'
config.cursor_vfx_particle_density = 0.7
```
