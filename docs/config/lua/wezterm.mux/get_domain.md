# `wezterm.mux.get_domain(name_or_id)`

{{since('20230320-124340-559cb7b0')}}

Resolves `name_or_id` to a domain and returns a
[MuxDomain](../MuxDomain/index.md) object representation of it.

`name_or_id` can be:

* A domain name string to resolve the domain by name
* A domain id to resolve the domain by id
* `nil` or omitted to return the current default domain
* other lua types will generate a lua error

If the name or id don't map to a valid domain, this function will return `nil`.

!!! note
    This is a snapshot of the domains registered at the moment of the call.
    On Windows, when [wsl_domains](../config/wsl_domains.md) is left
    unconfigured, the default WSL domain list is discovered in the
    background rather than blocking startup, so for the first moment or two
    after wezterm starts a WSL domain that hasn't been discovered yet will
    read as `nil` here. Spawning into such a domain by name does wait for
    discovery, though only briefly, so it is not a reliable substitute; it
    is only this lookup that doesn't wait at all. If you need a startup
    event handler to see a specific WSL domain deterministically, set
    `wsl_domains` explicitly.
