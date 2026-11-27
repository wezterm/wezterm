# `wezterm cli set-tab-var NAME [VALUE]`

{{since('nightly')}}

*Run `wezterm cli set-tab-var --help` to see more help*

Sets the tab variable `NAME` to `VALUE`, or removes it with `--unset`.

Tab variables are named values that a program sets to recognize a tab by later.
Unlike the tab's title, nothing running in the tab's panes can change them: escape
sequences that set titles leave them alone. They are kept by the multiplexer that
owns the tab, so a GUI attached to a multiplexer server sees the server's values,
and [wezterm cli list --format json](list.md) reports them as `tab_vars`. In Lua,
they are read with [tab:get_vars()](../../config/lua/MuxTab/get_vars.md) and set
with [tab:set_var()](../../config/lua/MuxTab/set_var.md).

```console
$ wezterm cli set-tab-var session.id 1f8a2c
$ wezterm cli list --format json | jq '.[] | {tab_id, tab_vars}'
{
  "tab_id": 0,
  "tab_vars": {
    "session.id": "1f8a2c"
  }
}
$ wezterm cli set-tab-var --unset session.id
```

## Synopsis

```console
{% include "../../examples/cmd-synopsis-wezterm-cli-set-tab-var--help.txt" %}
```
