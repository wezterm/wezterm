# `SelectorActions`

{{since('nightly')}}

Activates an overlay to display a list of choices for the
user to select from along with list of [SelectorActionsArgument](../SelectorActionsArgument.md)
objects 

`SelectorActions` accepts the following fields:

* `description` - text to display at the top of the menu
* `context` - an optional argument that accepts a
  [TransientContext](../TransientContext) object
* `choices` - list of strings to select from
* `section` - an [SelectorActionsArgumentSection](../SelectorActionsArgumentSection.md)
  object * `multiple` - this is an optional argument. Defaults to `false`.
  If set to `true`, user can select multiple choices.
* `fuzzy_description` - text to display when in fuzzy finding mode.
  This is an optional argument. Defaults to text mentioned against
  `description`
* `cancel` - event callback registered via `wezterm.action_callback`. The
  callback's function signature is `(window, pane)` where `window` and
  `pane` are the [Window](../window/index.md) and [Pane](../pane/index.md).
  This is an optional argument. If present, this callback is called when the
  user cancels the current overlay
