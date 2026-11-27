# `wezterm cli split-pane`

*Run `wezterm cli split-pane --help` to see more help*

Split the current pane.
Outputs the pane-id for the newly created pane on success.

This command will create a split in the current pane and spawn a command into it.  This splits the pane and creates a new one at the bottom running the default command:

```
$ wezterm cli split-pane
2
```

You may spawn an alternative program by passing the argument list; it is
recommended that you use `--` to denote the end of the arguments being passed
to `wezterm cli split-pane` so that any parameters you may wish to pass to the
program are not confused with parameters to `wezterm cli split-pane`.  This example
launches bash as a login shell in a new pane at the bottom:

```
$ wezterm cli split-pane -- bash -l
3
```

This example creates a split to the left, occupying 30% of the available space:

```
$ wezterm cli split-pane --left --percent 30
4
```

The following options affect the behavior:

* `--cwd CWD` - Specify the current working directory for the initially spawned program.
* `--horizontal` - Equivalent to `--right`. If neither this nor any other direction is specified, the default is equivalent to `--bottom`.
* `--pane-id` - Specifies the pane that should be split. See also [Targeting Panes](index.md#targeting-panes).


{{since('20220624-141144-bd1b7c5d')}}

* `--bottom` - Split vertically, with the new pane on the bottom.
* `--cells CELLS` - The number of cells that the new split should have. If omitted, 50% of the available space is used.
* `--left` - Split horizontally, with the new pane on the left.
* `--move-pane-id MOVE_PANE_ID` - Instead of spawning a new command, move the specified pane into the newly created split.
* `--percent PERCENT` - Specify the number of cells that the new split should have, expressed as a percentage of the available space.
* `--right` - Split horizontally, with the new pane on the right.
* `--top` - Split vertically, with the new pane on the top.
* `--top-level` - Rather than splitting the active pane, split the entire window.

## When there is no room for the split

Each part of a split needs at least one cell each way, beside the one-cell
divider. A split that leaves no room for that is refused before anything
changes: no program is started, and a pane named by `--move-pane-id` stays
where it was. A pane moved within its own tab is measured for on the tab
without it, since taking it out gives its room to its neighbours. The error
names the sizes; for `wezterm cli split-pane --pane-id 0 --bottom --cells 30
--move-pane-id 1` in an 80x24 tab, it is:

```
pane 1 cannot move beside pane 0, and stays where it was: no room for a pane of 30 cells below pane 0, which is 80x24 cells: the split would leave parts of 80x0 and 80x30 cells, and each needs at least 1x1 beside the divider
```

## Synopsis

```console
{% include "../../examples/cmd-synopsis-wezterm-cli-split-pane--help.txt" %}
```
