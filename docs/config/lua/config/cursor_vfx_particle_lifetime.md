---
tags:
  - appearance
  - text_cursor
---
# `cursor_vfx_particle_lifetime`

{{since('nightly')}}

Controls how long particles produced by
[cursor_trail_style](cursor_trail_style.md) persist on screen, in seconds.

Higher values create longer-lasting trails; lower values make particles
disappear quickly. The default is `0.35`.

```lua
config.cursor_trail_style = 'Torpedo'
config.cursor_vfx_particle_lifetime = 0.35
```
