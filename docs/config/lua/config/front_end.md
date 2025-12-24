---
tags:
  - gpu
---
# `front_end = "OpenGL"`

Specifies which render front-end to use.  This option used to have
more scope in earlier versions of wezterm, but today it allows four
possible values:

* `OpenGL` - use GPU accelerated rasterization
* `Software` - use CPU-based rasterization (uses llvmpipe/software GPU emulation)
* `WebGpu` - use GPU accelerated rasterization {{since('20221119-145034-49b9839f', inline=True)}}
* `Cairo2D` - pure 2D software rendering using Cairo, optimized for remote desktop/VNC environments

{{since('20240127-113634-bbcac864', outline=true)}}
    The default is `"WebGpu"`. In earlier versions it was `"OpenGL"`

{{since('20240128-202157-1e552d76', outline=true)}}
    The default has been reverted to `"OpenGL"`.

You may wish (or need!) to select `Software` if there are issues with your
GPU/OpenGL drivers.

WezTerm will automatically select `Software` if it detects that it is
being started in a Remote Desktop environment on Windows.

## WebGpu

{{since('20221119-145034-49b9839f')}}

The WebGpu front end allows wezterm to use GPU acceleration provided by
a number of platform-specific backends:

* Metal (on macOS)
* Vulkan
* DirectX 12 (on Windows)

See also:

* [webgpu_preferred_adapter](webgpu_preferred_adapter.md)
* [webgpu_power_preference](webgpu_power_preference.md)
* [webgpu_force_fallback_adapter](webgpu_force_fallback_adapter.md)

## Cairo2D

The `Cairo2D` front end provides pure 2D software rendering using the
[Cairo graphics library](https://www.cairographics.org/). Unlike the
`Software` front end which uses llvmpipe (a software OpenGL/GPU emulation
layer), Cairo2D bypasses GPU emulation entirely and renders directly to
a pixel buffer using efficient 2D operations.

### When to Use Cairo2D

Cairo2D is specifically designed for environments where GPU acceleration is
unavailable or impractical:

* **Terminal servers** - Multi-user servers where GPU resources are shared or unavailable
* **VNC/Remote desktop** - Connections where X11 forwarding or remote display protocols are used
* **Virtual machines** - Guests without GPU passthrough
* **Headless servers** - Systems without display hardware

In these scenarios, the `Software` front end forces the use of llvmpipe,
which emulates a full GPU in software. This is highly CPU-intensive and
inefficient for terminal rendering where most frames are largely static
text. Cairo2D avoids this overhead by rendering only what has changed.

### Performance Optimizations

Cairo2D implements several optimizations to minimize CPU usage and network
bandwidth:

#### Line-Based Dirty Region Tracking

Instead of re-rendering the entire screen on every frame, Cairo2D tracks
which terminal lines have changed using per-line content hashing. When
content changes, only the affected lines are re-rendered and transmitted.
This is particularly effective for:

* Scrolling output (only new lines are rendered)
* Cursor movement (only affected lines update)
* Partial screen updates (editing in vim, etc.)

#### Glyph Caching

Rendered glyphs are cached with their foreground color, background color,
and cell dimensions as the cache key. This means each unique character/color
combination is rendered once and reused, dramatically reducing CPU time for
static content.

#### Partial X11 Updates

When running over X11, Cairo2D uses targeted `PutImage` calls to update
only the dirty regions of the window. This reduces bandwidth consumption
by 50-95% compared to full-frame updates, which is critical for remote
desktop and VNC scenarios.

#### Frame Reuse

Complete frame hashes are computed to detect when the display content is
identical to the previous frame. In this case, the existing rendered surface
is presented without any re-rendering.

### Metrics

Cairo2D exports several metrics for monitoring performance:

* `cairo2d.efficiency_1s_pct` - Bandwidth savings over the last 1 second
* `cairo2d.efficiency_10s_pct` - Bandwidth savings over the last 10 seconds
* `cairo2d.efficiency_60s_pct` - Bandwidth savings over the last 60 seconds
* `cairo2d.cache.hit.rate` - Glyph cache hit rate
* `cairo2d.frame.reused.rate` - Frames reused without re-rendering
* `cairo2d.frame.partial.rate` - Frames with partial updates

### Platform Support

Currently, Cairo2D is supported on:

* **X11** (Linux) - Full support including partial updates

Wayland support is planned but not yet implemented.

### Example Configuration

```lua
-- For terminal server or VNC usage:
return {
  front_end = "Cairo2D",
}
```

### Comparison with Software Front End

| Aspect | Software (llvmpipe) | Cairo2D |
|--------|---------------------|---------|
| Rendering approach | Full GPU emulation | Pure 2D operations |
| CPU usage | High (GPU emulation overhead) | Low (direct pixel operations) |
| Incremental updates | No (full frame each time) | Yes (line-based dirty tracking) |
| Glyph caching | Via GPU texture atlas | Direct pixel cache per fg/bg color |
| Network bandwidth | Full frame data | Only changed regions |
| Best for | Compatibility fallback | Remote desktop/VNC/terminal servers |
