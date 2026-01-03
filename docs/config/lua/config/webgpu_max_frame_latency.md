---
tags:
  - gpu
---
# `webgpu_max_frame_latency = 2`

{{since('nightly')}}

Controls the maximum number of frames that the GPU can queue before blocking.

This option is only applicable when you have configured `front_end = "WebGpu"`.

Lower values reduce input latency but may reduce throughput. Higher values allow more frames to be queued, which can improve performance but increases input latency.

The default value is `2`.

## Example

To reduce input latency, set a lower frame latency:

```lua
local config = {}
config.front_end = 'WebGpu'
config.webgpu_present_mode = 'Mailbox'
config.webgpu_max_frame_latency = 1
return config
```

See also [webgpu_present_mode](webgpu_present_mode.md),
[webgpu_power_preference](webgpu_power_preference.md).
