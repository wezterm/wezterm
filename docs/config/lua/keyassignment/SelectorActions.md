# `SelectorActions`

{{since('nightly')}}

Activates an overlay to display a list of choices for the
user to select from along with list of [SelectorActionsArgument](../SelectorActionsArgument.md)
objects 

`SelectorActions` accepts the following fields:

* `description` - text to display at the top of the menu
* `context` - an optional argument that accepts a
  [TransientContext](../TransientContext.md) object
* `choices` - a lua table consisting of the potential choices. Each entry
  is itself a table with a `label` field and an optional `id` field.
  The label will be shown in the list, while the id can be a different
  string that is meaningful to your action. The label can be used together
  with [wezterm.format](../wezterm/format.md) to produce styled text.
* `section` - an [SelectorActionsArgumentSection](../SelectorActionsArgumentSection.md)
  object * `multiple` - this is an optional argument. Defaults to `false`.
  If set to `true`, user can select multiple choices.
* `fuzzy_description` - text to display when in fuzzy finding mode.
  This is an optional argument. Defaults to text mentioned against
  `description`
* `fuzzy` - a boolean that defaults to `false`. If `true`, SelectorActions will start
  in its fuzzy finding mode (this is equivalent to starting the SelectorActions and
  pressing <kbd>Ctrl</kbd> + <kbd>/</kbd> in the default mode)
* `cancel` - event callback registered via `wezterm.action_callback`. The
  callback's function signature is `(window, pane)` where `window` and
  `pane` are the [Window](../window/index.md) and [Pane](../pane/index.md).
  This is an optional argument. If present, this callback is called when the
  user cancels the current overlay


### Key Assignments

The default key assignments in the SelectorActions are as follows:

| Action  |  Key Assignment |
|---------|-------------------|
| Toggle fuzzy search | <kbd>Ctrl</kbd> + <kbd>/</kbd> |
| Add to filtering string (if in fuzzy finding mode) | Any key not listed below |
| Remove from filtering string (if in fuzzy finding mode) | <kbd>Backspace</kbd> |
| Move Down      | <kbd>Ctrl</kbd> + <kbd>N</kbd> |
|                | <kbd>Ctrl</kbd> + <kbd>J</kbd> |
| Move Up        | <kbd>Ctrl</kbd> + <kbd>P</kbd> |
|                | <kbd>Ctrl</kbd> + <kbd>K</kbd> |
| Select all filtered entries (if multiple enabled) | <kbd>Ctrl</kbd> + <kbd>A</kbd> |
| Deselect all filtered entries (if multiple enabled) | <kbd>Ctrl</kbd> + <kbd>D</kbd> |
| Toggle all filtered entries (if multiple enabled) | <kbd>Ctrl</kbd> + <kbd>T</kbd> |
| Toggle current entry and move down (if multiple enabled) | <kbd>Tab</kbd> |
| Toggle current entry and move up (if multiple enabled) | <kbd>Shift</kbd> + <kbd>Tab</kbd> |
| Quit     | <kbd>Ctrl</kbd> + <kbd>G</kbd> |
|          | <kbd>Ctrl</kbd> + <kbd>C</kbd> |
|          | <kbd>Escape</kbd> |
