---
tags:
  - keys
---
# `macos_forward_marked_text_to_pane`

Controls whether macOS marked text updates from an IME are forwarded to the
active pane as terminal input while composition is still in progress.

This is disabled by default. When enabled, WezTerm computes a text update from
the previous marked text to the new marked text. The update is sent to the pane
as DEL bytes followed by the newly added suffix. The final committed text is
reconciled with the forwarded marked text so that the pane receives one live
editing stream.

This can be useful for speech input methods that update marked text as partial
transcripts become available.

Use this for shell and readline-style prompts. Full-screen terminal
applications and raw-mode programs may receive the live text and DEL bytes
directly.

```lua
config.macos_forward_marked_text_to_pane = true
```
