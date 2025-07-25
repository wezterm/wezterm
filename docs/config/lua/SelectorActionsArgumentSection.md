# `SelectorActionsArgumentSection`

{{since('nightly')}}

`SelectorActionsArgumentSection` struct is a lua object with the following fields:
* `header` - text to display as header of the argument section
* `arguments` - list of [SelectorActionsArgument](./SelectorActionsArgument.md)
  objects


Example of `SelectorActionsArgument` object:

```lua
local positional_arg = {
  key = 'l',
  description = 'Logs',
  action = wezterm.action_callback(function(window, pane, result)
    wezterm.log_info(result)
  end),
}
```
