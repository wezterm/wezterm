---
tags:
  - clipboard
---
# `image_paste_local_path`

*Since: nightly builds only*

Specifies the local file path template used when pasting clipboard images
in local (non-SSH) panes. This is used by both the `PasteFrom` action's
image fallback and the `PasteImageToSshUpload` action's local fallback.

When the clipboard contains an image and the current pane is not an SSH
session, WezTerm saves the image to this path and pastes the resulting
file path into the terminal.

The `{timestamp}` placeholder is replaced with the current Unix timestamp
in seconds, ensuring unique file names for each paste operation.

The default value uses the platform's temporary directory:
* **Windows:** `%TEMP%\wezterm-paste-{timestamp}.png`
* **Linux/macOS:** `/tmp/wezterm-paste-{timestamp}.png`

Parent directories are created automatically if they don't exist.

```lua
-- Example: use a custom directory
config.image_paste_local_path = '/home/user/screenshots/wezterm-paste-{timestamp}.png'
```
