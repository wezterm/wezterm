# `window:set_var(NAME, VALUE)`

{{since('nightly')}}

Sets the window variable `NAME` to the string `VALUE`, or removes it when
`VALUE` is `nil`. See [window:get_vars()](get_vars.md).

When the window belongs to a multiplexer server that this GUI is attached to, the
variable is set on the server, where other clients and `wezterm cli` see it.

```lua
window:set_var('session.id', '1f8a2c')
window:set_var('session.id', nil)
```
