# `SelectorActionsArgument`

{{since('nightly')}}

The `SelectorActionsArgument` struct specifies information about the
action we are going to perform.

It is a lua object with the following fields:
* `key` - text to enter in order to trigger the argument
* `description` - text to describe the argument
* `action` - an event callback registered via `wezterm.action_callback`.  The
  callback's function signature is `(window, pane, result)` where `window` and
  `pane` are the [Window](./window/index.md) and [Pane](./pane/index.md)
  objects from the current pane and window, and `result` is a
  [SelectorActionsResult](./SelectorActionsResult.md) object
