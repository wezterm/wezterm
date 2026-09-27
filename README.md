# Wez's Terminal

<img height="128" alt="WezTerm Icon" src="https://raw.githubusercontent.com/wezterm/wezterm/main/assets/icon/wezterm-icon.svg" align="left"> *A GPU-accelerated cross-platform terminal emulator and multiplexer written by <a href="https://github.com/wez">@wez</a> and implemented in <a href="https://www.rust-lang.org/">Rust</a>*

User facing docs and guide at: https://wezterm.org/

![Screenshot](docs/screenshots/two.png)

*Screenshot of wezterm on macOS, running vim*

## About this fork

This is a personal fork of [wezterm/wezterm](https://github.com/wezterm/wezterm)
that is built for Windows. It adds the following on top of upstream:

### Wider dividers between panes

By default the divider between panes is a single cell wide, with the split
line drawn through its middle, so the text of adjacent panes ends up half a
cell away from the line. Two options make the dividers wider/taller:

```lua
config.pane_divider_cols = 3 -- width of the divider between left/right panes, in cells
config.pane_divider_rows = 2 -- height of the divider between top/bottom panes, in rows
```

* Both default to `1` (the upstream behavior) and accept values from `1` to `8`.
* The split line is drawn through the middle of the divider, and the whole
  divider can be dragged with the mouse to resize the panes.
* Changing the values and reloading the configuration re-arranges the panes
  of the open tabs, keeping their proportions.
* With multiplexer domains (`wezterm connect`, unix and ssh domains) the
  panes are laid out by the mux server, which must use the same values.

See [pane_divider_cols](docs/config/lua/config/pane_divider_cols.md) and
[pane_divider_rows](docs/config/lua/config/pane_divider_rows.md).

### Built-in session saving (resurrect)

The [resurrect.wezterm](https://github.com/MLFlexer/resurrect.wezterm) plugin,
which saves and restores windows, tabs, panes and their text, is compiled into
wezterm, so it works without downloading anything:

```lua
local resurrect = require("resurrect")
```

Do not also load it with `wezterm.plugin.require`. Compared to the upstream
plugin, the bundled version:

* has no runtime dependency on the third party `dev.wezterm` plugin;
* doesn't spawn helper processes (`mkdir`, VBS scripts via `wscript.exe`,
  `find`), except for the optional encryption with `age`/`rage`/`gpg`;
* saves the state in `%LOCALAPPDATA%\wezterm\resurrect\` on Windows
  (`<wezterm data dir>/resurrect/` elsewhere);
* works with the wider pane dividers described above.

The saved state contains the text of the panes in plain text unless
encryption is enabled. See
[lua-api-crates/resurrect/README.md](lua-api-crates/resurrect/README.md)
for a complete configuration example (key bindings to save and restore,
periodic saving, restoring the last session on startup) and the list of
changes.

### Getting a build

The fork only builds for Windows, using GitHub Actions:

* every pull request runs the `windows` workflow (build, tests, packaging);
* every push to `main` that touches the code runs the `windows_continuous`
  workflow.

Open the run on the [Actions](https://github.com/dbevdev/wezterm/actions)
page and download the `windows` artifact from the *Artifacts* section (you
need to be signed in to GitHub). It contains the installer
(`WezTerm-*-setup.exe`) and a portable zip (`WezTerm-windows-*.zip`); you only
need one of them.

To install, close all wezterm windows (and `wezterm-mux-server.exe`, if you
use multiplexer domains) and run the installer. It is not code signed, so
Windows SmartScreen will warn about it: choose *More info → Run anyway*. The
installer replaces an existing installation of the official wezterm; to go
back, install the official release over it. The portable zip can be unpacked
anywhere and run side by side with an installed version.

The other upstream workflows (Linux, macOS, Nix, formatting, docs) can be
started manually from the Actions page.

## Installation

https://wezterm.org/installation

## Getting help

This is a spare time project, so please bear with me.  There are a couple of channels for support:

* You can use the [GitHub issue tracker](https://github.com/wezterm/wezterm/issues) to see if someone else has a similar issue, or to file a new one.
* Start or join a thread in our [GitHub Discussions](https://github.com/wezterm/wezterm/discussions); if you have general
  questions or want to chat with other wezterm users, you're welcome here!
* There is a [Matrix room via Element.io](https://matrix.to/#/#wezterm:matrix.org)
  for (potentially!) real time discussions.

The GitHub Discussions and Element/Gitter rooms are better suited for questions
than bug reports, but don't be afraid to use whichever you are most comfortable
using and we'll work it out.

## Supporting the Project

If you use and like WezTerm, please consider sponsoring it: your support helps
to cover the fees required to maintain the project and to validate the time
spent working on it!

[Read more about sponsoring](https://wezterm.org/sponsor.html).

* [![Sponsor WezTerm](https://img.shields.io/github/sponsors/wez?label=Sponsor%20WezTerm&logo=github&style=for-the-badge)](https://github.com/sponsors/wez)
* [Patreon](https://patreon.com/WezFurlong)
* [Ko-Fi](https://ko-fi.com/wezfurlong)
* [Liberapay](https://liberapay.com/wez)
