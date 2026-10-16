---
tags:
  - performance
---
# `glyph_cache_image_cache_bytes = 268435456`

Specifies the combined byte budget (256 MiB by default) for the decoded
images held by each window's render-side image cache.

When the budget is exceeded, the least frequently displayed images are
dropped from the cache.  Images that are still shown on screen are
re-wrapped from the terminal model the next time they are drawn, so
this bounds memory usage rather than what can be displayed.

Lowering this can significantly reduce memory usage in sessions that
display many large images (for example, a terminal PDF viewer that
streams each page as an image).

```lua
return {
  glyph_cache_image_cache_bytes = 128 * 1024 * 1024,
}
```

See also [kitty_image_storage_budget_mb](kitty_image_storage_budget_mb.md)
for the terminal-side storage quota and
[image_dedup_cache_budget_bytes](image_dedup_cache_budget_bytes.md)
for the payload dedup cache.
