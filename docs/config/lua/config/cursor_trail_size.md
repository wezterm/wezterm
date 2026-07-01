---
tags:
  - appearance
  - text_cursor
---
# `cursor_trail_size`

{{since('nightly')}}

Controls how much the trailing edge of the [cursor_smear](cursor_smear.md)
lags behind the leading edge, determining the visible length of the stretched
cursor body.

* `1.0` (default) — maximum trail. The leading corners jump immediately to
  the destination while the trailing corners animate at the full
  [cursor_animation_duration](cursor_animation_duration.md) duration, producing
  the longest and most dramatic smear.
* `0.5` — moderate trail. Leading and trailing corners meet somewhere between
  the start and the full animation duration.
* `0.0` — no visible trail. All four corners animate at the same speed,
  moving the cursor as a rigid body.

Has no effect when `cursor_smear = false`.

```lua
config.cursor_smear = true
config.cursor_trail_size = 1.0
```
