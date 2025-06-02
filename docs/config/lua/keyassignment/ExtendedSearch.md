---
tags:
  - search
---

# `ExtendedSearch`

{{since('nightly')}}

This extends the functionality of [Search](./Search.md)

This action will trigger the search overlay for the current tab.

`ExtendedSearch` accepts two parameters:
* `pattern` - allows below options
    * `{ CaseSensitiveString = '' }`
    * `{ CaseInSensitiveString = '' }`
    * `{ Regex = '' }`
    * `"CurrentSelectionOrEmptyString"`
* `activate_match` - a string parameter indicating the match to activate. Valid values are `AfterCursor`, `BeforeCursor`, `First`.

The supported [regular expression syntax is described
here](https://docs.rs/regex/1.3.9/regex/#syntax).


```lua
local act = wezterm.action

config.keys = {
  -- search for things that look like git hashes with activated match after the current cursor position
  {
    key = 'H',
    mods = 'SHIFT|CTRL',
    action = act.ExtendedSearch {
      pattern = { Regex = '[a-f0-9]{6,}' },
      activate_match = 'AfterCursor',
    },
  },
  -- search for the lowercase string "hash" matching the case exactly and activate the match before the current cursor position
  {
    key = 'H',
    mods = 'SHIFT|CTRL',
    action = act.ExtendedSearch {
      pattern = { CaseSensitiveString = 'hash' },
      activate_match = 'BeforeCursor',
    },
  },
  -- search for the string "hash" matching regardless of case and activate the first match
  {
    key = 'H',
    mods = 'SHIFT|CTRL',
    action = act.ExtendedSearch {
      pattern = { CaseInSensitiveString = 'hash' },
      activate_match = 'First',
    },
  },
  -- search for the current selection and activate the match after the current cursor position
  {
    key = 'H',
    mods = 'SHIFT|CTRL',
    action = act.ExtendedSearch {
      pattern = 'CurrentSelectionOrEmptyString',
      activate_match = 'AfterCursor',
    },
  },
}
```

[Learn more about the search overlay](../../../scrollback.md#searching-the-scrollback)

The selection text is adjusted to be a single line.
