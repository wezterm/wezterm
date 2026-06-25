---
tags:
  - windows
---

## Windows Default Terminal

{{since('nightly')}}

Starting with Windows 11 22H2 and Windows 10 22H2 (build 19045.3031,
KB5026435), the inbox `conhost.exe` can delegate an incoming console
session to an out-of-process COM server selected by the user.
Microsoft ships `WindowsTerminal.exe` to fill that role; wezterm can fill
the same role, so launching a console application from the Start Menu,
Explorer, or the `Run` dialog opens a wezterm window instead of a bare
console window.

The full architecture is described in
[Microsoft's spec #492](https://github.com/microsoft/terminal/blob/main/doc/specs/%23492%20-%20Default%20Terminal/spec.md).

## Requirements

This feature is only available on Windows.  You need one of:

* Windows 11 22H2 or later
* Windows 10 22H2, build 19045.3031 or later (with KB5026435 applied)

No extra build flags or cargo features are required.  The bundled
`OpenConsole.exe` console host and `OpenConsoleProxy.dll` marshalling
stub (both from Microsoft's Windows Terminal release, MIT-licensed) are
copied next to `wezterm-gui.exe` at build time.  See
[Building from source](../install/source.md) for Windows build
instructions.

## Registering wezterm as the default terminal

```console
> wezterm terminal-host register
```

This writes the per-user registry values that point `conhost.exe` at
wezterm, and registers `wezterm-gui.exe` as the local COM server so it
can be launched on demand when no wezterm is already running.
After registering, console applications open in wezterm.

To undo the registration:

```console
> wezterm terminal-host unregister
```

!!! note
    On machines that don't have Windows Terminal installed, `register`
    also registers the bundled `OpenConsole.exe` under the Microsoft
    OpenConsole CLSID as a fallback ConPTY host.  Without that fallback,
    console application launches would fail with `0xc0000142`
    (`STATUS_DLL_INIT_FAILED`).  `unregister` removes the fallback only
    when it was originally installed by wezterm.

The `register` subcommand accepts two opt-out flags for advanced use:

* `--no-local-server` - skip writing the `LocalServer32` entry for
  wezterm-gui.exe (use if you manage COM registration yourself).
* `--no-proxy-stub` - skip per-user registration of the bundled
  `OpenConsoleProxy.dll` (use if you already have a system-wide
  marshalling stub registered).

## Showing the current default and known hosts

```console
> wezterm terminal-host list
```

This prints the current default terminal, a table of known hosts (with
whether each is installed and which is the current default), and any
MSIX-packaged terminal apps installed for the current user.

## Choosing a different default host

```console
> wezterm terminal-host set-default wezterm
> wezterm terminal-host set-default conhost
> wezterm terminal-host set-default wt-release
```

The available host ids are:

| Id           | Name                           |
|--------------|--------------------------------|
| `conhost`    | Windows Console Host (classic) |
| `wt-release` | Windows Terminal (Release)     |
| `wt-preview` | Windows Terminal (Preview)     |
| `wt-canary`  | Windows Terminal (Canary)      |
| `wt-dev`     | Windows Terminal (Dev)         |
| `wezterm`    | WezTerm                        |

Raw CLSIDs are intentionally not accepted; use one of the ids above.

## Resetting to "Let Windows decide"

```console
> wezterm terminal-host reset
```

This clears the per-user default and lets Windows pick (typically
`conhost.exe`).

## Verifying the registration

You can inspect the registry directly to confirm the registration:

```console
> reg query "HKCU\Console\%%Startup"

HKEY_CURRENT_USER\Console\%%Startup
    DelegationConsole    REG_SZ    {2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}
    DelegationTerminal    REG_SZ    {8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}

> reg query "HKCU\Software\Classes\CLSID\{8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}\LocalServer32"

HKEY_CURRENT_USER\Software\Classes\CLSID\{8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}\LocalServer32
    (Default)    REG_SZ    "C:\Program Files\WezTerm\wezterm-gui.exe"
```

## How it works

When a console application is launched, Windows boots `conhost.exe`,
which reads two `REG_SZ` values from `HKCU\Console\%%Startup`:

| Value                | Purpose                                                |
|----------------------|--------------------------------------------------------|
| `DelegationConsole`  | CLSID of the COM server that hosts the ConPTY          |
| `DelegationTerminal` | CLSID of the COM server that provides the terminal UX  |

WezTerm registers itself under `DelegationTerminal`.  The ConPTY side
(`DelegationConsole`) is satisfied either by an installed Windows
Terminal, or by the bundled `OpenConsole.exe` registered as a fallback
when `register` runs.

When a console handoff arrives and no wezterm process is already
running, the COM Service Control Manager launches `wezterm-gui.exe`
with an `-Embedding` flag.  WezTerm strips that flag before its CLI
parser sees it, registers the termhost COM class on the main thread,
and waits for the handoff callback.  The incoming PTY in/out/signal
handles are then attached to a new tab in a new window via the normal
mux machinery.

## See also

* [Microsoft Default Terminal spec (#492)](https://github.com/microsoft/terminal/blob/main/doc/specs/%23492%20-%20Default%20Terminal/spec.md)
* [`ITerminalHandoff.idl`](https://github.com/microsoft/terminal/blob/main/src/host/proxy/ITerminalHandoff.idl)
* [Installing on Windows](../install/windows.md)
