---
title: wezterm.default_wsl_domains
tags:
 - wsl
 - multiplexing
---

# wezterm.default_wsl_domains()

{{since('20220319-142410-0fcdea07')}}

Computes a list of [WslDomain](../WslDomain.md) objects, each one
representing an installed
[WSL](https://docs.microsoft.com/en-us/windows/wsl/about) distribution
on your system.

This list is the same as the default value for the
[wsl_domains](../config/wsl_domains.md) configuration option, which is to make
a `WslDomain` with the `distribution` field set to the name of WSL distro and the
`name` field set to name of the distro but with `"WSL:"` prefixed to it.

For example, if:

```
; wsl -l -v
  NAME            STATE           VERSION
* Ubuntu-18.04    Running         1
```

then this function will return:

```
{
  {
    name: "WSL:Ubuntu-18.04",
    distribution: "Ubuntu-18.04",
  },
}
```

The purpose of this function is to aid in situations where you might want set
`default_prog` for the WSL distributions:

```lua
local wezterm = require 'wezterm'

local wsl_domains = wezterm.default_wsl_domains()

for idx, dom in ipairs(wsl_domains) do
  if dom.name == 'WSL:Ubuntu-18.04' then
    dom.default_prog = { 'fish' }
  end
end

return {
  wsl_domains = wsl_domains,
}
```

However, wez strongly recommends that you use `chsh` inside the WSL domain to make
that the default shell if possible, so that you can avoid this additional configuration!

{{since('20230320-124340-559cb7b0')}}

The `default_cwd` field is now automatically set to `"~"` to make it more
convenient to launch a WSL instance in the home directory of the configured
distribution.

!!! note
    This function always computes a fresh, live list by running `wsl -l -v`,
    and its result is never cached, so every call pays that cost again --
    several seconds, and longer on a cold WSL start. If you use the recipe
    above of calling `wezterm.default_wsl_domains()` directly from your
    config, that means paying it synchronously every time your config is
    evaluated, including on every reload. If you don't need a live answer,
    consider setting `wsl_domains` once with an explicit, hand-written list
    instead of calling this function from your config.

    This is a different function from the one wezterm itself uses
    internally to populate the default `wsl_domains` list when it isn't
    configured explicitly: that internal discovery runs off the startup
    path in the background and its result is cached for the life of the
    process, so it doesn't pay this cost on every spawn or every reload the
    way a config that calls this function directly would. One consequence
    of that internal caching is that a long-lived process (in particular
    `wezterm-mux-server`) won't notice a WSL distro installed or removed
    after it started, even across a config reload, unless it's restarted.
    If you need that, call `wezterm.default_wsl_domains()` directly from
    your config (as in the recipe above, or even just `wsl_domains =
    wezterm.default_wsl_domains()` with no filtering) to get a fresh
    enumeration on every reload instead. Note that this makes *newly
    installed* distributions show up, but does not make *removed* ones go
    away: domains already registered with the multiplexer are never
    unregistered, so a domain for a since-removed distribution remains
    listed until the process restarts, and attempting to spawn into it
    will not do what you want.
