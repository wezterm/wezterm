---
tags:
  - multiplexing
---
# `kitty_image_storage_budget_mb = 320`

Specifies the per-terminal storage quota, in MiB, for image data
retained on behalf of the
[kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

The protocol spec recommends that terminals impose a storage quota and
free the oldest images once it is exceeded; a client that references a
released image id will receive an `ENOENT` error response.  Images that
are still displayed on the screen or retained in the scrollback are
never released.

Setting this to `0` releases image data as soon as it is no longer
referenced by a placement.

```lua
return {
  kitty_image_storage_budget_mb = 128,
}
```

See also [glyph_cache_image_cache_bytes](glyph_cache_image_cache_bytes.md)
and [image_dedup_cache_budget_bytes](image_dedup_cache_budget_bytes.md).
