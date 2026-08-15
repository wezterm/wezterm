---
tags:
  - gpu
  - appearance
---
# `custom_shaders = {}`

{{since('nightly')}}

Specifies a list of post-processing shaders that are applied to the rendered
terminal image after the terminal content has been drawn.  Each shader runs
as a full-screen fragment pass and can read the rendered terminal as an input
texture.  Shaders are applied in order; with more than one shader, the output
of each pass becomes the input to the next.

This option currently requires `front_end = "WebGpu"`.

## Native WGSL shaders

A native shader is a `.wgsl` file that you provide directly.  Your shader
must provide a `@fragment` entry point named `fs_postprocess` that reads from
`screen_texture` using `screen_sampler` and returns the final color.

The following uniforms are available to your shader via the `pp` uniform
buffer:

* `resolution` (`vec2<f32>`) - pixel dimensions of the rendered output
* `time` (`f32`) - seconds since the shader pipeline started
* `time_delta` (`f32`) - seconds since the previous frame
* `frame` (`u32`) - frame counter

## Example

```lua
config.front_end = 'WebGpu'
config.custom_shaders = {
  '/absolute/path/to/my_effect.wgsl',
  'shaders/another_effect.wgsl', -- relative to the config file
}
```