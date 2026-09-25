# `wezterm.mux.rename_workspace(old, new)`

{{since('20230408-112425-69ae8472')}}

Renames the workspace *old* to *new*.

```lua
local wezterm = require 'wezterm'
local active = wezterm.mux.get_active_workspace()

wezterm.mux.rename_workspace(active, 'something different')
```
