---
tags:
  - transient
---

# `SelectorActionsResult`

{{since('nightly')}}

The `SelectorActionsResult` struct is a lua object with the following fields:
* `choices` - a lua table consisting of the selected choices. Each entry
  is itself a table with a `label` field and an optional `id` field.


Example of `SelectorActionsResult` object:

```lua
local result = {
  choices = {
    { label = 'choice1', id = 'random_id' },
    { label = 'choice2' },
  },
}
```
