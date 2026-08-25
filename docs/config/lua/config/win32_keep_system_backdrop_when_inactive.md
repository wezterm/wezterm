---
tags:
  - appearance
---

# `win32_keep_system_backdrop_when_inactive = false`

{{since('nightly')}}

By default, when a window using a
[win32_system_backdrop](win32_system_backdrop.md) material (*Acrylic*, *Mica*
or *Tabbed*) becomes inactive, DWM stops rendering the backdrop material and
fades it to a solid fallback color, making the window look opaque until it is
focused again.

Setting this option to `true` keeps the configured backdrop material rendered
while the window is inactive, so the window stays translucent with the same
effect regardless of focus:

```lua
config.window_background_opacity = 0
config.win32_system_backdrop = 'Acrylic'
config.win32_keep_system_backdrop_when_inactive = true
```

This works by reporting the window to DWM as always active for non-client
rendering purposes (the `WM_NCACTIVATE` state). A side effect is that, when
using a `window_decorations` style that shows the native title bar, the title
bar will always render in its active colors.

This option has no effect when `win32_system_backdrop` is `"Auto"` or
`"Disable"`, and is only meaningful on Windows.
