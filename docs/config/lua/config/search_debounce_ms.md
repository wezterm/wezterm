---
tags:
  - tuning
---

# `search_debounce_ms = 350`

{{since('nightly')}}

Specifies the debounce duration in milliseconds for search updates after a key press.
This avoids performance issues when the first few characters of a search string result in 
a large number of matches. A value of `0` will disable the delay. The default is `350`.

```lua
config.search_debounce_ms = 0
```