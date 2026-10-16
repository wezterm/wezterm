---
tags:
  - performance
---
# `kitty_image_disk_cache_budget_bytes = 536870912`

Specifies the total size, in bytes, that the on-disk spill cache for
kitty graphics images may occupy when
[enable_kitty_image_disk_cache](enable_kitty_image_disk_cache.md)
is enabled.

Once the budget is exceeded, the least recently used spilled images
are removed from disk; a client that later places one of their ids
observes an `ENOENT` error response, just as it would have without
the cache.

A single image larger than the whole budget is still cached until
another image needs the space.

```lua
return {
  enable_kitty_image_disk_cache = true,
  kitty_image_disk_cache_budget_bytes = 128 * 1024 * 1024,
}
```

See also [enable_kitty_image_disk_cache](enable_kitty_image_disk_cache.md),
[kitty_image_disk_cache_dir](kitty_image_disk_cache_dir.md)
and [kitty_image_storage_budget_mb](kitty_image_storage_budget_mb.md).
