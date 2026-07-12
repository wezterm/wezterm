---
tags:
  - search
  - navigation
---
# `search_trigger_on_navigate`

Controls whether WezTerm's search overlay automatically re-runs the search when new output is printed in the terminal.

When set to `true`, background updates from streaming terminal outputs will not automatically re-run the search query. Instead, the search is only updated when you modify the search pattern (e.g. typing or deleting text) or actively navigate the search matches (e.g. using `PriorMatch` or `NextMatch` key assignments).

This prevents flickering of search highlights and keeps the scrollbar/viewport locked at your current viewing position during fast-flowing outputs.

The default is `false`, which enables eager and automatic background updates.

```lua
config.search_trigger_on_navigate = true
```
