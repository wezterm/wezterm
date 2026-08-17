---
tags:
  - multiplexing
  - wsl
---
# `wsl_domains`

{{since('20220319-142410-0fcdea07')}}

Configures [WSL](https://docs.microsoft.com/en-us/windows/wsl/about) domains.

This option accepts a list of [WslDomain](../WslDomain.md) objects.

The default is a list derived from parsing the output of `wsl -l -v`.  See
[wezterm.default_wsl_domains()](../wezterm/default_wsl_domains.md) for more
about that list, and on how to override it.

!!! note
    When left unconfigured, that default list is discovered in the
    background rather than blocking startup on `wsl -l -v`, so it (and the
    corresponding domains) may not be immediately available in the first
    moment or two after wezterm starts. A `default_domain`,
    `default_mux_server_domain` or `--domain` naming one of them waits for
    discovery to finish rather than failing, however long that takes --
    in the instance that is starting up. `wezterm start --domain` handed
    off to an *already running* instance is an ordinary by-name spawn
    over there, and so gets the brief wait described next; if that
    instance is itself only a few seconds old and hasn't finished
    discovering yet, the hand-off is declined and you get a new window
    from a new process instead of a tab in the existing one.
    Spawning into one by name from anywhere else -- a key assignment, or
    `wezterm.mux.spawn_window{domain=...}` -- also waits, but only briefly,
    and reports the name as invalid if discovery hasn't found it by then;
    if you need a specific WSL domain guaranteed available that early, set
    `wsl_domains` explicitly rather than relying on auto-discovery. The
    discovered list is also cached for the lifetime of the process: WSL
    distributions installed or removed after that first discovery won't be
    picked up until wezterm is restarted. If you want newly installed
    distributions to be picked up on a config reload instead, set
    `wsl_domains = wezterm.default_wsl_domains()`, which enumerates them
    afresh every time your config is evaluated; see
    [wezterm.default_wsl_domains()](../wezterm/default_wsl_domains.md) for
    what that costs, and for why removed distributions still don't go away.
