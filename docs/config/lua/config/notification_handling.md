---
tags:
  - notifications
---

# `notification_handling = "AlwaysShow"`

{{since('20240127-113634-bbcac864')}}

This option controls how wezterm behaves when a toast notification escape
sequence is received.

The following escape sequences will generate a toast notification:

```console
$ printf "\e]777;notify;%s;%s\e\\" "title" "body"
```

```console
$ printf "\e]9;%s\e\\" "hello there"
```

This configuration option can have one of the following values,
which have the following effects:

 * `AlwaysShow` - Show the notification regardless of the current focus
 * `NeverShow` - Never show the notification
 * `SuppressFromFocusedPane` - Show the notification unless it was generated from the currently focused pane
 * `SuppressFromFocusedTab` - Show the notification unless it was generated from the currently focused tab
 * `SuppressFromFocusedWindow` - Show the notification unless it was generated from the currently focused window

{{since('nightly', inline=True)}}
The `SuppressFromFocused*` values only suppress the notification while the
wezterm window that shows the pane has the keyboard focus. If you have
switched to a different application, the notification is always shown.
Previously the notification was also suppressed for the pane, tab or window
that most recently had the focus, even after switching to a different
application.

## Clicking on the notification

{{since('nightly')}}

On Windows, clicking on the body of the notification brings the wezterm
window that contains the pane that generated it to the front, and activates
that pane and its tab. Nothing happens if the pane has since been closed.
If the pane belongs to a workspace that isn't the active one, the pane and
its tab are activated but the workspace is not switched, so no window is
brought to the front, unless
[toast_notification_click_switches_workspace](toast_notification_click_switches_workspace.md)
is enabled.

## One notification per pane

{{since('nightly')}}

On Windows, a pane has at most one notification at a time: a new
notification replaces the one that the same pane generated earlier, rather
than accumulating in the notification center. The notification is removed
when you move to the pane that generated it, and when that pane is closed.

## Changing how the notification looks

{{since('nightly')}}

The [format-toast-notification](../window-events/format-toast-notification.md)
event allows changing the text of the notification, or preventing it from
being shown, and
[toast_notification_duration](toast_notification_duration.md) controls how
long it stays on screen on Windows.
[toast_notification_silent](toast_notification_silent.md) turns off the
notification sound of Windows.
