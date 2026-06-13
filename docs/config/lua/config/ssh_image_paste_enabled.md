---
tags:
  - ssh
---
# `ssh_image_paste_enabled = true`

*Since: nightly builds only*

Controls whether the
[PasteImageToSshUpload](../keyassignment/PasteImageToSshUpload.md)
key assignment is active. When set to `false`, the action silently
does nothing.

```lua
-- Disable SSH image paste feature
config.ssh_image_paste_enabled = false
```
