---
tags:
  - appearance
  - text_cursor
---
# `cursor_trail_style`

{{since('nightly')}}

Selects a particle or highlight effect that plays when the cursor moves at
least [cursor_trail_min_distance](cursor_trail_min_distance.md) cells.

The default is `nil` (no particles). The smear effect
([cursor_smear](cursor_smear.md)) is available independently and can be used
with or without a `cursor_trail_style`.

## Available styles

### Particle styles

These spawn particles along the travel path on each qualifying move.

| Value | Description |
|---|---|
| `"Torpedo"` | Particles scatter opposing the direction of travel. Recommended. |
| `"PixieDust"` | Small squares that fall under gravity. Recommended. |
| `"Railgun"` | Sinusoidal particle stream fanning along the movement vector. |

### Highlight styles

These spawn a single expanding shape at the cursor destination.

| Value | Description |
|---|---|
| `"SonicBoom"` | Expanding filled square. |
| `"Ripple"` | Expanding hollow ring. |
| `"Wireframe"` | Expanding hollow rectangle. |

## Example

```lua
config.cursor_trail_style = 'Torpedo'
```

## Related options

* [cursor_trail_min_distance](cursor_trail_min_distance.md) — minimum
  movement required to trigger the effect.
* [cursor_vfx_opacity](cursor_vfx_opacity.md) — particle/highlight opacity.
* [cursor_vfx_particle_lifetime](cursor_vfx_particle_lifetime.md) — how long
  particles persist.
* [cursor_vfx_particle_density](cursor_vfx_particle_density.md) — particles
  spawned per cell of movement.
* [cursor_vfx_particle_speed](cursor_vfx_particle_speed.md) — initial
  particle speed.
* [cursor_vfx_particle_size](cursor_vfx_particle_size.md) — particle
  diameter.
