# `tab:get_vars()`

{{since('nightly')}}

Returns a table of the tab's variables: the names and values set with
[tab:set_var()](set_var.md) or `wezterm cli set-tab-var`. Nothing running in
the tab's panes can change them, so a program can recognize the tab by them
where its title would not do.

```lua
local vars = tab:get_vars()
if vars['session.id'] then
  wezterm.log_info('this tab is session ' .. vars['session.id'])
end
```
