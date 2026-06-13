---
tags:
  - ssh
---
# `ssh_image_paste_remote_path = "/tmp/wezterm-paste-{timestamp}.png"`

*Since: nightly builds only*

Specifies the remote file path template used when uploading clipboard images
via the [PasteImageToSshUpload](../keyassignment/PasteImageToSshUpload.md)
key assignment.

The `{timestamp}` placeholder is replaced with the current Unix timestamp
in seconds, ensuring unique file names for each paste operation.

```lua
config.ssh_image_paste_remote_path = '/tmp/wezterm-paste-{timestamp}.png'
```
