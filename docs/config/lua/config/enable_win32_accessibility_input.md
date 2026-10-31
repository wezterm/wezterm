---
tags:
  - keys
---
# `enable_win32_accessibility_input`

{{since('nightly')}}

Windows only. Defaults to `false`. Set this option before creating a window:

```lua
config.enable_win32_accessibility_input = true
```

Exposes a writable UI Automation input element at the active terminal cursor.
This allows dictation software such as Typeless to recognize an input target.
Changes to this option require a new window; existing windows retain their
initial setting.

The element represents an immediately consumed input buffer. Its value and
text range are empty because committed text is sent to the terminal through
the normal paste path. It does not expose the terminal screen, scrollback,
shell command line, or terminal selection. Setting its value inserts text;
it cannot replace text previously sent to an application. No Enter key is
added, and the receiving application's normal paste behavior applies.

Input requests are accepted only for the focused window and active pane.
This is experimental input assistance, not full screen-reader support or
Windows Text Services Framework support. Windows voice typing (Win+H) is
not covered by this option.
