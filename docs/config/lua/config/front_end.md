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

Cairo2D is designed for environments where GPU acceleration is unavailable
or impractical:

* **Terminal servers** - Multi-user servers where GPU resources are shared or unavailable
* **VNC/Remote desktop** - Connections where X11 forwarding or remote display protocols are used
* **Virtual machines** - Guests without GPU passthrough
* **Headless servers** - Systems without display hardware

In these scenarios, the `Software` front end forces the use of llvmpipe,
which emulates a full GPU in software. Cairo2D avoids this overhead by
using direct 2D rendering with incremental updates.

### Platform Support

Currently, Cairo2D is supported on:

* **X11** (Linux) - Full support including partial updates

Wayland, macOS, and Windows support is planned but not yet implemented.

### Example Configuration

```lua
return {
  front_end = "Cairo2D",
}
```
