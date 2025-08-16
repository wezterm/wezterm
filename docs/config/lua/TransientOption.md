---
tags:
  - transient
---

# `TransientOption`

{{since('nightly')}}

The `TransientOption` struct specifies information about a command line
flag that can be toggled and requires a value when activated.

It is a lua object with the following fields:
* `key` - text to enter in order to set option
* `default` - optional argument indicating default value.
  If omitted, option is not set
* `description` - text to describe the option
* `flag` - text that is passed against label in the callback
  when an argument is activated
* `allow_nil` - optional argument that determines whether to allow
  setting the option to `nil` if previously set to a string.
  If omitted, option is set to false if previously set.
  Else, user is prompted for value
* `choices` - Optional argument indicating the list of choices
  to select from. If provided, a selector is displayed
  when setting an option. Else, a line prompt is displayed
  If omitted, when setting an option, a line prompt is displayed
  when setting an option


Example of `TransientOption` object:

```lua
local option = {
  key = '-t',
  default = '100',
  description = 'Tail',
  flag = '--tail=',
  allow_nil = true,
  choices = { 'choice1', 'choice2' },
}
```
