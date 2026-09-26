---
tags:
  - appearance
---
# `pane_divider_cols = 1`

{{since('nightly')}}

Specifies the width, in cells, of the divider between panes that are
arranged side by side (left and right), such as those created by
[SplitHorizontal](../keyassignment/SplitHorizontal.md).

The default is `1`, which means that the divider occupies a single column,
with the split line drawn through its middle.  Larger values leave more
space between the text of adjacent panes and the split line, which can
make the panes easier to tell apart.  The split line is always drawn
through the middle of the divider, and the whole divider can be dragged
with the mouse to resize the panes.

The value must be in the range `1` to `8`; values outside that range are
clamped and a warning is logged.

```lua
config.pane_divider_cols = 3
```

The height of the divider between panes that are stacked on top of each
other is controlled separately by
[pane_divider_rows](pane_divider_rows.md).

When the configuration is reloaded with a different value, the panes of
existing tabs are re-arranged to fit around the revised dividers.  The
value is taken from the global configuration; per-window overrides set via
[window:set_config_overrides](../window/set_config_overrides.md) are not
taken into account.

!!! note
    Pane layout for [multiplexing domains](../../../multiplexing.md)
    (`wezterm connect`, unix and ssh domains) is computed by the mux server
    rather than by the GUI.  The server and the GUI must be configured with
    the same divider sizes, otherwise the panes will be mis-positioned.
    Only the default value of `1` is supported with a mux server that
    doesn't know about this option.
