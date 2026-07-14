---
tags:
  - appearance
---

# `win32_system_backdrop_keep_inactive = false`

{{since('nightly')}}

When a [win32_system_backdrop](win32_system_backdrop.md) material is in use,
DWM swaps the backdrop for a desaturated/solid *inactive* fallback whenever
the window is deactivated — a power-saving behavior that cannot be disabled
system-wide.

Setting this option to `true` keeps the vivid *active* material rendering
while the window is unfocused, by reporting the window's non-client visual
state as active. Real focus handling is unaffected.

```lua
config.window_background_opacity = 0.75
config.win32_system_backdrop = "Acrylic"
config.win32_system_backdrop_keep_inactive = true
```

Defaults to `false`, matching the OS behavior.

Has no effect when `win32_system_backdrop = "Disable"`.

See [#5895](https://github.com/wezterm/wezterm/issues/5895) for the
motivating issue.
