---
tags:
  - appearance
---

# `window_appearance = "System"`

{{since('nightly')}}

Controls whether the window uses a light or dark appearance, independently
of the appearance selected for the system as a whole.

Possible values are:

* `"System"` - follow the appearance of the system. This is the default.
* `"ForcedDark"` - always use a dark appearance for the window.
* `"ForcedLight"` - always use a light appearance for the window.

For example, if your system uses a light appearance for applications but
you'd like WezTerm to look dark:

```lua
config.window_appearance = 'ForcedDark'
```

This setting affects:

* The native window decorations, such as the title bar, on Windows and macOS.
* On X11, WezTerm sets the `_GTK_THEME_VARIANT` property of the window, which
  is honored by some window managers, such as Mutter, to choose the variant of
  the window decorations.
* The value returned by
  [window:get_appearance()](../window/get_appearance.md).

When the system uses a high contrast appearance, forcing the appearance
only changes whether it is light or dark: `window:get_appearance()` then
returns `"DarkHighContrast"` or `"LightHighContrast"`.

This setting doesn't change your [color scheme](../../appearance.md), and it
doesn't affect
[wezterm.gui.get_appearance()](../wezterm.gui/get_appearance.md), which
always reports the appearance of the system. That means that a configuration
that picks its color scheme based on `wezterm.gui.get_appearance()` keeps
following the system. If you'd like your color scheme to follow
`window_appearance` instead, choose it based on `window:get_appearance()`,
as shown in its documentation.

This setting can be overridden for an individual window by using
[window:set_config_overrides()](../window/set_config_overrides.md).
