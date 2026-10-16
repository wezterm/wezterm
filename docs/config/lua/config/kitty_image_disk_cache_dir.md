---
tags:
  - performance
---
# `kitty_image_disk_cache_dir = nil`

Specifies the parent directory under which the on-disk spill cache
for kitty graphics images creates its per-process temporary
directory, when [enable_kitty_image_disk_cache](enable_kitty_image_disk_cache.md)
is enabled.

By default (or when set to `nil`) the system temp dir (respecting
`$TMPDIR`) is used.  The directory is created by wezterm and removed
again when the process exits; the option names a location, not the
cache directory itself.  Changing it takes effect the next time
wezterm starts.

Use this to place the cache on a filesystem with more space or
faster IO; note that a tmpfs-backed temp dir keeps the "disk" cache
in RAM.

```lua
return {
  enable_kitty_image_disk_cache = true,
  kitty_image_disk_cache_dir = "/var/tmp",
}
```

See also [enable_kitty_image_disk_cache](enable_kitty_image_disk_cache.md),
[kitty_image_disk_cache_budget_bytes](kitty_image_disk_cache_budget_bytes.md)
and [kitty_image_storage_budget_mb](kitty_image_storage_budget_mb.md).
