# `window:get_vars()`

{{since('nightly')}}

Returns a table of the window's variables: the names and values set with
[window:set_var()](set_var.md) or `wezterm cli set-window-var`. Nothing running in
the window's panes can change them, so a program can recognize the window by them
where its title would not do.

```lua
local vars = window:get_vars()
if vars['session.id'] then
  wezterm.log_info('this window is session ' .. vars['session.id'])
end
```
