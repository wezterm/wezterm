---
title: window_close_mux_behavior
---

{{since('nightly')}}

This option controls the behavior when a window is closed.

It specifically applies to domains that are "detachable" (such as
[Unix Domains](../unix_domains.md),
[SSH Domains](../ssh_domains.md) and
[TLS Domains](../tls_servers.md)).

When the behavior is set to `"Detach"`, the default, closing the window
will detach the associated domain if there are no other windows remaining
for that domain.  This effectively keeps the domain running in the background.

When set to `"Kill"`, the domain will be killed instead of detached.

```lua
config.window_close_mux_behavior = 'Kill'
```
