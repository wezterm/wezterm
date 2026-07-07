---
tags:
  - appearance
  - text_cursor
---
# `cursor_vfx_particle_size`

{{since('nightly')}}

Controls the maximum diameter of particles produced by
[cursor_trail_style](cursor_trail_style.md), expressed as a fraction of the
cell width.

* `1.0` — particles are as wide as one full cell.
* `0.5` (default) — particles are half a cell wide.
* `0.0` — invisible.

For `Torpedo` and `Railgun`, particle size shrinks proportionally to
remaining lifetime, so particles appear to fade out as they age. For
`PixieDust`, size is constant throughout the particle's lifetime.

```lua
config.cursor_trail_style = 'PixieDust'
config.cursor_vfx_particle_size = 0.5
```
