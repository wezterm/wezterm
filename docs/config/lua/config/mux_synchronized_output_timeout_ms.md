---
tags:
  - multiplexing
---
# `mux_synchronized_output_timeout_ms = 1000`

{{since('nightly')}}

Specifies how long, in milliseconds, an application may keep a synchronized
update (DEC private mode 2026) open before wezterm stops waiting for the
closing sequence and applies the buffered output anyway.

While a synchronized update is open, wezterm holds back output so that the
pane can be repainted as a single atomic frame. Without a bound on that hold,
an application that opens an update and then stalls would freeze its pane
indefinitely. When the timeout expires the buffered output is applied and the
eventual closing sequence is a no-op.

You should not normally need to change this. Raising it gives slow
applications more time to complete an atomic frame; lowering it makes wezterm
give up on stalled updates sooner. Setting it to `0` expires every update
immediately, which effectively disables synchronized output handling.
