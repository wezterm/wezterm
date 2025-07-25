# `SelectorActionsResult`

{{since('nightly')}}

The `SelectorActionsResult` struct is a lua object with the following fields:
* `entries` - list of selected choices


Example of `SelectorActionsResult` object:

```lua
local result = {
  choices = {
    'choice1',
    'choice2',
  },
}
```
