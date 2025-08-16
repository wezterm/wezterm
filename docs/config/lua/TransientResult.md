---
tags:
  - transient
---

# `TransientResult`

{{since('nightly')}}

The `TransientResult` struct is a lua object with the following fields:
* `entries` - list of [TransientResultEntry](#transientresultentry) objects


### `TransientResultEntry`

`TransientResultEntry` struct is a lua object with the following fields:
* `flag` - text passed against flag in one of the below entries:
  * [TransientSwitch](./TransientSwitch.md)
  * [TransientOption](./TransientOption.md)
  * [TransientCyclicSwitch](./TransientCyclicSwitch.md)
* `value` - the value selected for entity with above flag


Example of `TransientResult` object:
```lua
local result = {
  entries = {
    { flag = '--follow', value = true },
    { flag = '--tail=', value = '100' },
  },
}
```
