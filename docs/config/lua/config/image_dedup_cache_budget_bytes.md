---
tags:
  - performance
---
# `image_dedup_cache_budget_bytes = 134217728`

Specifies the byte budget (128 MiB by default) of the terminal-side
content-addressed cache that lets identical image re-transmissions
share one copy of the decoded payload.

Evicting from this cache has no effect on protocol correctness: an
identical re-transmission simply allocates a fresh copy of the payload
instead of sharing one.  A small number of recent entries is always
retained so that clients working with a small set of images keep
hitting the dedup path.

```lua
return {
  image_dedup_cache_budget_bytes = 64 * 1024 * 1024,
}
```

See also [glyph_cache_image_cache_bytes](glyph_cache_image_cache_bytes.md)
for the render-side image cache and
[kitty_image_storage_budget_mb](kitty_image_storage_budget_mb.md) for
the kitty graphics storage quota.
