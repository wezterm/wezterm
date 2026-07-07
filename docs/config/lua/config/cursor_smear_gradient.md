---
tags:
  - appearance
  - text_cursor
---
# `cursor_smear_gradient`

{{since('nightly')}}

Controls whether the [cursor_smear](cursor_smear.md) effect uses a gradient.

* `false` (default) — the smear body is rendered at a uniform opacity, giving
  a solid stretched-cursor look.
* `true` — the tail of the smear fades from fully transparent to the full
  cursor colour at the head, giving a comet-like effect.

Has no effect when `cursor_smear = false`.

```lua
config.cursor_smear = true
config.cursor_smear_gradient = true
```
