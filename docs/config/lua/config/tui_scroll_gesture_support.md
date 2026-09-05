---
tags:
  - mouse
---
# `tui_scroll_gesture_support = true`

{{since('nightly', outline=true)}}

When using terminal user interface (TUI) applications such as `tmux`, `vim`,
`nvim` or `emacs` that enable mouse reporting, wezterm forwards scroll wheel
events to the application.

Traditionally, mouse protocols understood only discrete wheel "ticks", without
any concept of larger scroll deltas. On systems with touchpads or devices such
as the Apple Magic Mouse, a single gesture can produce a large scroll delta.
By default wezterm would forward this as a single wheel tick, which makes
scrolling inside TUIs feel slow.

When `tui_scroll_gesture_support` is enabled, wezterm will expand larger scroll
deltas into multiple discrete wheel events before forwarding them to the
application. This allows TUIs to experience accelerated scrolling, while still
preserving single-step precision for small movements.

The default value is `false`.
