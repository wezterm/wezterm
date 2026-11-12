## Installing on macOS

The CI system builds the package on macOS Big Sur and should run on systems as
"old" as Mojave.  It may run on earlier versions of macOS, but that has not
been tested.

Starting with version 20210203-095643-70a364eb, WezTerm is a Universal binary
with support for both Apple Silicon and Intel hardware.

[:simple-apple: Download for macOS :material-tray-arrow-down:]({{ macos_zip_stable }}){ .md-button }
[:simple-apple: Nightly for macOS :material-tray-arrow-down:]({{ macos_zip_nightly }}){ .md-button }

1. Download <a href="{{ macos_zip_stable }}">Release</a>.
2. Extract the zipfile and drag the `WezTerm.app` bundle to your `Applications` folder.
3. First time around, you may need to right click and select `Open` to allow launching
   the application that you've just downloaded from the internet.
3. Subsequently, a simple double-click will launch the UI.
4. To use wezterm binary from a terminal emulator, like `wezterm ls-fonts` you'll need to add the location to the wezterm binary folder that exists _inside_ the WezTerm.app, to your environment's $PATH value. For example, to add it to your `~/.zshrc` file, and assuming your WezTerm.app was installed to `/Applications`, add:
```sh
PATH="$PATH:/Applications/WezTerm.app/Contents/MacOS"
export PATH
```
5. Configuration instructions can be [found here](../config/files.md)

## Homebrew

WezTerm is available for [brew](https://brew.sh/) users:

```console
$ brew install --cask wezterm
```

If you'd like to use a nightly build:

```console
$ brew install --cask wezterm@nightly
```

!!! note
    For users who have previously used the cask named `wezterm-nightly`,
    homebrew has started issuing warnings: `Warning: Cask
    homebrew/cask-versions/wezterm-nightly was renamed to wezterm@nightly`. We
    recommend that you use `brew uninstall wezterm-nightly` to uninstall the
    previously installed version, and then reinstall the new version using the
    command above.

to upgrade to a newer nightly (normal `brew upgrade` will not upgrade it!):

```console
$ brew upgrade --cask wezterm@nightly --greedy-latest
```

!!! note
    The `--greedy-latest` option in Homebrew forces the latest version of a
    formula to be installed, even if a version satisfying the formula's
    requirements is already installed. This can be useful when you want to
    ensure you have the most up-to-date version of a package, regardless of
    whether an older version meets the current dependency requirements.

## MacPorts

WezTerm is also available via [MacPorts](https://ports.macports.org/port/wezterm/summary):

```console
$ sudo port selfupdate
$ sudo port install wezterm
```

## Building and installing from a local checkout

If you maintain your own fork (or want to run a build of your local tree),
the `Makefile` can build and install a proper `WezTerm.app` bundle:

```console
$ make install-app          # builds release binaries, assembles dist/WezTerm.app, installs to /Applications
```

Notes:

* The bundle is ad-hoc signed (`codesign --sign -`); since it was built locally,
  macOS Gatekeeper won't quarantine it.
* The installed bundle is independent of any `~/.local/bin` CLI copies you may
  have made; refresh either after pulling new changes:
  `make install-app` for the app, `cp target/release/{wezterm,wezterm-gui,wezterm-mux-server} ~/.local/bin/` for the CLIs.
* `make app` builds the bundle into `dist/` without installing it.
* Homebrew's `wezterm` cask is not required; if it is installed, remove it first
  with `brew uninstall --cask wezterm` to avoid two competing installations.

