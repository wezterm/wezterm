---
tags:
  - appearance
  - text_cursor
---
# `cursor_trail_min_distance`

{{since('nightly')}}

Specifies the minimum cursor movement in cells required to trigger the
[cursor_trail_style](cursor_trail_style.md) particle or highlight effect.

The default is `4`.

Increasing this value means effects only fire on large jumps (e.g. search
results, `gg`/`G`, `Ctrl-f`), keeping the display quiet during normal
navigation. Decreasing it towards `1` fires the effect on nearly every move.

Has no effect when `cursor_trail_style` is not set.

```lua
config.cursor_trail_style = 'Torpedo'
config.cursor_trail_min_distance = 4
```
