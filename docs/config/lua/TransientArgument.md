# `TransientArgument`

{{since('nightly')}}

The `TransientArgument` struct specifies information about the
action we are going to perform.

It is a lua object with the following fields:
* `key` - text to enter in order to trigger the argument
* `description` - text to describe the argument
* `action` - an event callback registered via `wezterm.action_callback`.  The
  callback's function signature is `(window, pane, result)` where `window` and
  `pane` are the [Window](./window/index.md) and [Pane](./pane/index.md)
  objects from the current pane and window, and `result` is a
  [TransientResult](./TransientResult.md) object


Example of `TransientArgument` object:

```lua
local positional_arg = {
  key = 'l',
  description = 'Logs',
  action = wezterm.action_callback(function(window, pane, result)
    wezterm.log_info(result)
  end),
}
```
