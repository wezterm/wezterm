---
tags:
    - appearance
---
# ext_background_effect_v1 = false 

When combined with `window_background_opacity`, enables background blur
using the Wayland background effect protocol.

This can be used to produce a translucent window effect rather than
a crystal clear transparent window effect.

This effect can be achieved by adding the following to the configuration:
```lua
    config.window_background_opacity = 0.4
    config.ext_background_effect_v1 = true
```

[Screenshot](../../../screenshots/wezterm-ext-background-effects-v1.png)
See also [kde_window_background_blur](./kde_window_background_blur.md) for
a similar effect using the kde protocol

See also [win32_system_backdrop](./win32_system_backdrop.md) for a similar
effect on Windows

See also [macos_window_background_blur](./macos_window_background_blur.md) for
a similar effect on macOS
