---
tags:
  - appearance
  - text_cursor
---
# `cursor_animation_duration`

{{since('nightly')}}

Specifies the duration of the [cursor_smear](cursor_smear.md) animation in
seconds. This is the time it takes for the trailing edge of the smear to reach
the destination after a multi-cell cursor jump.

The default value is `0.15` (150 ms).

Setting this to `0.0` disables the position animation entirely, making all
cursor moves instantaneous even when `cursor_smear = true`.

```lua
config.cursor_smear = true
config.cursor_animation_duration = 0.15
```

The relative lag between the leading and trailing edges of the smear is
controlled separately by [cursor_trail_size](cursor_trail_size.md).
