# `augment-tab-bar-menu`

{{since('nightly')}}

This event is emitted when the tab bar's right-click menu opens (see
[ShowContextMenu](../keyassignment/ShowContextMenu.md)): on a tab, or on an
empty part of the tab bar. Its purpose is to add entries to that menu.

The first event parameter is a [`window` object](../window/index.md) that
represents the gui window.

The second event parameter is a [`pane` object](../pane/index.md) that
represents the active pane in the window.

The third event parameter is the index of the tab that was right-clicked,
counting from 0 as [ActivateTab](../keyassignment/ActivateTab.md) does, or
`nil` when the click was not on a tab.

This hook is synchronous; calling asynchronous functions, such as
[wezterm.run_child_process](../wezterm/run_child_process.md), will not succeed.
An action the menu runs, such as one made with
[wezterm.action_callback](../wezterm/action_callback.md), can call them.

The return value is a list of entries, shown after a separator below the menu's
own entries, in order. Each entry is one of:

* `{ label = 'Text', action = <action> }` - an item: choosing it closes the
  menu and performs the action, which can be any key assignment action. The
  item shows the shortcut the key bindings have for that action, if any.
* `{ label = 'Text', submenu = { <entries> } }` - a submenu: a list of entries
  of the same kinds, shown beside the menu when the pointer is over its label,
  or when it is highlighted and <kbd>RightArrow</kbd> or <kbd>Enter</kbd> is
  pressed. A submenu must have at least one item or submenu.
* `{ separator = true }` - a line between entries.

Returning `nil` or an empty list adds nothing. Anything else, such as an entry
without a label, an entry with both an action and a submenu, a key that is not
one of `label`, `action`, `submenu` and `separator`, or an action that is not
a key assignment, is reported as a configuration error naming the entry and
what is wrong with it, and the menu opens without the added entries, as it
does when the handler raises an error.

Only the first handler registered for this event is called.

## Adding a submenu of snippets

In this example, the tab bar's menu gets a submenu that types one of two
snippets into the active pane and, when the click was on a tab, an item that
switches to that tab:

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

local config = wezterm.config_builder()

-- wezterm.action_callback registers an event each time it is called, so the
-- callback is made once, rather than each time the menu opens.
local greeting = wezterm.action_callback(function(window, pane)
  pane:send_paste('Hello from ' .. wezterm.hostname())
end)

wezterm.on('augment-tab-bar-menu', function(window, pane, tab_index)
  local entries = {
    {
      label = 'Snippets',
      submenu = {
        { label = 'Git status', action = act.SendString 'git status\r' },
        { separator = true },
        { label = 'Greeting', action = greeting },
      },
    },
  }
  if tab_index then
    table.insert(entries, {
      label = 'Switch to This Tab',
      action = act.ActivateTab(tab_index),
    })
  end
  return entries
end)

return config
```
