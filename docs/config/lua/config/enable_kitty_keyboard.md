---
tags:
  - keys
---
# `enable_kitty_keyboard = true`

{{since('20220624-141144-bd1b7c5d')}}

When set to `true`, wezterm will honor kitty keyboard protocol escape
sequences that modify the [keyboard encoding](../../key-encoding.md).

{{since('nightly')}}
This option now defaults to `true`, aligning wezterm with kitty
itself: applications can negotiate the enhanced keyboard encoding
without any user configuration. Set it to `false` to restore the
older opt-in behavior.


