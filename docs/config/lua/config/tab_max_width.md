---
tags:
  - tab_bar
---
# `tab_max_width`

Specifies the maximum width that a tab can have in the
tab bar when using retro tab mode.  It is ignored when
using fancy tab mode.

Defaults to 16 glyphs in width.

When the titles do not all fit in the tab bar, the width is shared: a tab
whose title is shorter than an even share keeps its full width, and the
cells it leaves are divided among the longer titles.

```lua
config.tab_max_width = 16
```
