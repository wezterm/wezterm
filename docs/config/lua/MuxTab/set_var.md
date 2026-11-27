# `tab:set_var(NAME, VALUE)`

{{since('nightly')}}

Sets the tab variable `NAME` to the string `VALUE`, or removes it when
`VALUE` is `nil`. See [tab:get_vars()](get_vars.md).

When the tab belongs to a multiplexer server that this GUI is attached to, the
variable is set on the server, where other clients and `wezterm cli` see it.

```lua
tab:set_var('session.id', '1f8a2c')
tab:set_var('session.id', nil)
```
