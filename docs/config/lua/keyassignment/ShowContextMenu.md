# `ShowContextMenu`

{{since('nightly')}}

Opens the context menu at the mouse pointer. The menu offers the clipboard
(Copy, Paste), the pane (Split Right, Split Down, Close Pane), New Tab and the
Command Palette, each with the shortcut currently assigned to it. Copy is
available only while there is a selection.

The menu is drawn by wezterm itself rather than by the operating system, so it
looks and behaves the same on every platform and with every front end.

It is the default action for a right-click in the terminal area:

```lua
config.mouse_bindings = {
  {
    event = { Down = { streak = 1, button = 'Right' } },
    mods = 'NONE',
    action = wezterm.action.ShowContextMenu,
  },
}
```

Like the other default mouse assignments, it does not apply while an
application has turned on mouse reporting; hold `SHIFT` (see
[bypass_mouse_reporting_modifiers](../config/bypass_mouse_reporting_modifiers.md))
to open the menu over such an application. To turn it off, bind the same event
to `wezterm.action.DisableDefaultAssignment`.

A right-click on the tab bar opens a menu for the tab bar instead: New Tab,
Close Tab when the click was on a tab, then Minimize, Maximize or Restore, and
Close Window. The configuration can add entries to it, submenus among them,
with the [augment-tab-bar-menu](../window-events/augment-tab-bar-menu.md)
event.

A submenu's entry shows `▸`; pointing at it opens the submenu beside the menu,
on its right, or on its left where the window has no room on the right.

| Action | Key |
|--------|-----|
| Highlight the previous or next entry | <kbd>UpArrow</kbd>, <kbd>DownArrow</kbd> |
| Choose the highlighted entry | <kbd>Enter</kbd> |
| Open the highlighted submenu at its first entry | <kbd>RightArrow</kbd>, <kbd>Enter</kbd> |
| Close the innermost open submenu | <kbd>LeftArrow</kbd>, <kbd>Esc</kbd> |
| Close the menu | <kbd>Esc</kbd> with no submenu open, or a click outside it |
