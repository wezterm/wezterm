# `TransientMenu`

{{since('nightly')}}

This creates an overlay with keyboard driven menu similar to
Emacs transient menus. 

We can view and set switches, options, cyclic switches and
select an argument to trigger action with the current state
of above-mentioned entities passed as an argument.


`TransientMenu` accepts the following fields:

* `description` - text to display at the top of the menu
* `context` - an optional argument that accepts a [TransientContext](../TransientContext.md)
  object
* `sections` - list of [TransientSection](#transientsection) objects
* `cancel` - event callback registered via `wezterm.action_callback`. The
  callback's function signature is `(window, pane)` where `window` and
  `pane` are the [Window](../window/index.md) and [Pane](../pane/index.md).
  This is an optional argument. If present, this callback is called when the
  user cancels the current overlay


### `TransientSection`

`TransientSection` struct is a lua object with the following fields:
* `header` - text to describe the section
* `entries` - list of [TransientEntry](#transiententry) objects


### `TransientEntry`

`TransientEntry` struct is a lua object for which the possible values are:
* `{ TransientSwitch = obj }` where `obj` is a [TransientSwitch](../TransientSwitch.md)
  object
* `{ TransientOption = obj }` where `obj` is a [TransientOption](../TransientOption.md)
  object
* `{ TransientCyclicSwitch = obj }` where `obj` is a
  [TransientCyclicSwitch](../TransientCyclicSwitch.md) object
* `{ TransientArgument = obj }` where `obj` is a [TransientArgument](../TransientArgument.md)
  object
