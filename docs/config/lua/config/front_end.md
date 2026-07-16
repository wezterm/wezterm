---
tags:
  - gpu
---
# `front_end = "OpenGL"`

Specifies which render front-end to use.  This option used to have
more scope in earlier versions of wezterm, but today it allows these
possible values:

* `OpenGL` - use GPU accelerated rasterization
* `Software` - use CPU-based rasterization.
* `WebGpu` - use GPU accelerated rasterization {{since('20221119-145034-49b9839f', inline=True)}}
* `Gdi` - Windows-only text renderer that draws directly to the window with
  GDI (`ExtTextOutW`). Intended for Remote Desktop (RDP) sessions. See
  [the Gdi section](#gdi) below.

{{since('20240127-113634-bbcac864', outline=true)}}
    The default is `"WebGpu"`. In earlier versions it was `"OpenGL"`

{{since('20240128-202157-1e552d76', outline=true)}}
    The default has been reverted to `"OpenGL"`.

You may wish (or need!) to select `Software` if there are issues with your
GPU/OpenGL drivers.

On Windows, when `front_end` is left unset and WezTerm detects that it is
running inside a Remote Desktop (RDP) session, it defaults to `"Software"`
(as in prior releases). The new `"Gdi"` renderer is much lighter over RDP but is
currently opt-in (see below). An explicit `front_end` in your config always
wins.

!!! note "Behavior change"
    In an RDP session, an explicit `front_end = "OpenGL"` is now honored
    (previously it was silently forced to software rendering). OpenGL over RDP
    can behave poorly on disconnect; prefer `"Software"` or `"Gdi"` there.

## Gdi

The `Gdi` front end is a Windows-only text renderer that paints terminal
output directly to the window device context using GDI (`ExtTextOutW`),
without creating any OpenGL/WebGpu context.

Its purpose is Remote Desktop: RDP remotes GDI text/glyph drawing operations
as text, whereas the GPU front ends (and `Software`) present an opaque
framebuffer that RDP must screen-scrape and video-encode every frame. In a
typical RDP/Hyper-V session that exposes no 3D GPU this makes `Gdi`
substantially more responsive and much lighter on network bandwidth, similar
to how Windows Terminal behaves over RDP.

It is Windows-only and currently opt-in; request it explicitly:

```lua
config.front_end = "Gdi"
```

Fonts and cell metrics in `Gdi` mode come from GDI (`GetTextMetrics`) for the
configured primary font family and `font_size`, so glyph positioning may differ
slightly from the GPU front ends; this is intentional to guarantee that glyphs
fit the grid.

To reduce RDP traffic, `Gdi` only repaints lines that changed since the last
frame (tracked via per-line sequence numbers), plus the cursor's previous/next
cell and rows whose selection changed. Scrolling, resizing, focus changes and
config reloads trigger a full repaint.

### Supported in `Gdi` mode

* Truecolor foreground/background, reverse video
* Bold / italic / underline / strikethrough
* Wide (CJK) cells
* Block / bar / underline cursor (hollow block when unfocused)
* Selection highlight
* A minimal tab strip built from `format-tab-title`

### Not supported in `Gdi` mode (MVP limitations)

* Ligatures / HarfBuzz shaping (drawing is per-cell)
* Images (sixel / iTerm / kitty), custom shaders, background images,
  blur/retro effects, and animated cursor/visual bell
* IME/composition rendering is best-effort
* Overlay/modal UI that is drawn via the GPU box-model — the command palette and
  the character/pane selectors — is not yet rendered in `Gdi` mode (it depends on
  the GPU glyph atlas). Note that *overlay panes* (launcher, copy-mode, the
  fuzzy selector, prompts) render normally, and tab-bar mouse clicks work.
* On non-Windows platforms, `Gdi` falls back to `OpenGL` with a warning.

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
