# `wezterm check-config`

{{since('nightly')}}

Loads your configuration the way starting wezterm would, reports anything wrong with it, and exits with a non-zero
status if it could not be loaded. No window is opened.

```console
$ wezterm check-config
config ok: /home/wez/.config/wezterm/wezterm.lua
```

By default the configuration file that wezterm would normally load is checked. You may name a different one:

```console
$ wezterm check-config /path/to/wezterm.lua
```

!!! warning
    Wezterm exports `WEZTERM_CONFIG_FILE` to the programs it runs, and that path wins over the normal search.
    So running `wezterm check-config` with no file named, inside a wezterm pane, checks whatever
    file that instance loaded. Name a file explicitly when you want to be certain which one is checked.

## The simulated display

No window system is running during the check, so the parts of `wezterm.gui` that describe a display report a fixed,
simulated environment rather than your machine:

| Function | Reports |
| --- | --- |
| `wezterm.gui.get_appearance()` | `Light`, override by passing `--appearance` |
| `wezterm.gui.screens()` | one screen named `simulated`, 1920x1080 pixels at the origin, scale 1.0, 96 DPI |

Everything else in `wezterm.gui` behaves normally. `enumerate_gpus()` enumerates whatever adapters the checking machine
has. `gui_windows()` and `gui_window_for_mux_window()` need a running window and raise an error, as they would
this early in a real startup.

!!! note
    A configuration branch that only runs under a different appearance is never evaluated, so an error inside it
    will not be found. Check both:

    ```console
    $ wezterm check-config --appearance light
    $ wezterm check-config --appearance dark
    ```

`--appearance` also accepts `light-high-contrast` and `dark-high-contrast`.

## Exit status

The command exits `0` when your configuration file was evaluated without raising an error and produced a usable
configuration. It exits non-zero otherwise: if the file cannot be opened, has a syntax error, raises an error while it runs,
returns nothing or something that is not a configuration table, sets an option to an unusable value, or produces key
assignments that cannot be resolved.

Warnings are printed but do not by themselves cause a non-zero exit. Pass `--warnings-as-errors` to exit `1` when any
warning was produced.

If you name no file and none is found, the built-in defaults are checked and the command exits `0`, because they are a
valid configuration. It warns on stderr that it checked the defaults, so `--warnings-as-errors` turns it into a failure.

## Testing a plugin

A wezterm configuration file is an ordinary lua script, so a file that checks its own assumptions and then returns a
configuration is a test suite, and this command runs it:

```lua
local wezterm = require 'wezterm'
local my_plugin = require 'my_plugin'

assert(
  my_plugin.format_title { title = 'x' } == ' x ',
  'title should be padded'
)

return {}
```

Anything the script raises becomes the failure this command reports, so a failed assertion exits `1`.

Lua test frameworks generally require the `debug` module, which wezterm does not otherwise make available:

```console
$ wezterm check-config --unsafe-enable-debug-module spec/init.lua
```

!!! warning
    Enabling `debug` is unsafe: it allows changing upvalues, locals and metatables that the Rust side of wezterm owns,
    so a configuration that misuses it can crash wezterm, opening a surface for exploits. Only enable debug when
    testing trusted code. Prefer testing in an isolated environment that doesn't have access to any secrets.

## Synopsis

```console
{% include "../examples/cmd-synopsis-wezterm-check-config--help.txt" %}
```
