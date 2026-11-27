# `wezterm cli set-window-var NAME [VALUE]`

{{since('nightly')}}

*Run `wezterm cli set-window-var --help` to see more help*

Sets the window variable `NAME` to `VALUE`, or removes it with `--unset`.

Window variables are named values that a program sets to recognize a window by
later. Unlike the window's title, which a program in any of its panes can set with
an escape sequence, nothing running in the window can change them. They are kept
by the multiplexer that owns the window, so a GUI attached to a multiplexer server
sees the server's values, and [wezterm cli list --format json](list.md) reports
them as `window_vars`. In Lua, they are read with
[window:get_vars()](../../config/lua/mux-window/get_vars.md) and set with
[window:set_var()](../../config/lua/mux-window/set_var.md).

```console
$ wezterm cli set-window-var project.group work
$ wezterm cli list --format json | jq '.[] | {window_id, window_vars}'
{
  "window_id": 0,
  "window_vars": {
    "project.group": "work"
  }
}
$ wezterm cli set-window-var --unset project.group
```

## Synopsis

```console
{% include "../../examples/cmd-synopsis-wezterm-cli-set-window-var--help.txt" %}
```
