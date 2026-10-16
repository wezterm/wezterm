---
tags:
  - performance
---
# `enable_kitty_image_disk_cache = false`

Enables an on-disk spill cache for images received via the
[kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

The disk cache is the destination for images that were released
because [kitty_image_storage_budget_mb](kitty_image_storage_budget_mb.md)
was exceeded: instead of being dropped, their pixels are written to
the cache, and a client that later places the same image id gets the
pixels restored rather than an `ENOENT` error response.  Images that
a client explicitly deletes are never cached.

The cache only ever holds data that overflowed a memory budget; it is
not used unconditionally.  It lives in a per-process temp directory
(under [kitty_image_disk_cache_dir](kitty_image_disk_cache_dir.md),
or the system temp dir) that is removed when wezterm exits, and its
total size is bounded by
[kitty_image_disk_cache_budget_bytes](kitty_image_disk_cache_budget_bytes.md).

When a client application's alternate-screen session ends (ie: a
full-screen application such as a terminal PDF viewer exits), the
image data it left behind is released from memory *and* removed from
this disk cache: a relaunched application retransmits everything it
displays, so retained copies would never pay off.

Spilling to and restoring from disk happens synchronously while
processing the graphics escape sequences, so it is worth observing
the `image_disk_cache.*` metrics before keeping the feature enabled.
Nothing is printed by default; either set the `periodic_stat_logging`
option to a non-zero number of seconds to get a periodic table on
stderr, or query `wezterm.metrics.get_latency()`,
`wezterm.metrics.get_sizes()` and `wezterm.metrics.get_counters()`
from lua.  Of particular interest
are the read/write latency and size histograms and the
`image_disk_cache.restore` / `image_disk_cache.restore_miss`
counters: if restores are rare, the cache is costing writes without
saving re-transmissions.

```lua
return {
  enable_kitty_image_disk_cache = true,
}
```

See also [kitty_image_storage_budget_mb](kitty_image_storage_budget_mb.md),
[kitty_image_disk_cache_budget_bytes](kitty_image_disk_cache_budget_bytes.md)
and [kitty_image_disk_cache_dir](kitty_image_disk_cache_dir.md).
