# `wezterm.mux.all_domains()`

{{since('20230320-124340-559cb7b0')}}

Returns an array table holding all of the known
[MuxDomain](../MuxDomain/index.md) objects.

!!! note
    This is a snapshot of the domains registered at the moment of the call.
    On Windows, when [wsl_domains](../config/wsl_domains.md) is left
    unconfigured, the default WSL domain list is discovered in the
    background rather than blocking startup, so for the first moment or two
    after wezterm starts any WSL domain that hasn't been discovered yet is
    simply absent from this list. If you need a startup event handler to see
    a specific WSL domain deterministically, set `wsl_domains` explicitly.
