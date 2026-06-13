# `PasteImageToSshUpload`

*Since: nightly builds only*

Reads an image from the clipboard, uploads it to the remote server via SFTP
(or SCP as fallback), and pastes the remote file path into the current pane.

On Windows, the clipboard image is read as DIB format and converted to PNG.
On Linux (X11 and Wayland), the clipboard image is read directly as PNG via
the `image/png` MIME type.
On macOS, the clipboard image is read as PNG (`public.png`) if available,
otherwise as TIFF (`public.tiff`) and converted to PNG.

This action is designed for use when connected to a remote host via an SSH
domain and you want to share a screenshot or clipboard image with a remote
application (such as Claude Code) that can read images from file paths.

The remote path is determined by
[ssh_image_paste_remote_path](../config/ssh_image_paste_remote_path.md).

The feature can be disabled via
[ssh_image_paste_enabled](../config/ssh_image_paste_enabled.md).

**Behavior by context:**

| Clipboard | Pane type | Action |
|-----------|-----------|--------|
| Image | SSH (domain) | Upload via SFTP, paste remote path |
| Image | SSH (detected process) | Upload via SCP, paste remote path |
| Image | Local (non-SSH) | Save to local file, paste local path |
| Text only | Any | Paste text directly |
| Empty | Any | Show error toast |

When the pane is not connected via SSH, the image is saved locally using the
path template from
[image_paste_local_path](../config/image_paste_local_path.md).

**Requirements:**
* Windows, Linux (X11/Wayland), or macOS
* The clipboard must contain image data or text
* On Linux/FreeBSD: `xclip` or `xsel` (X11), or `wl-paste` from
  `wl-clipboard` (Wayland) must be installed. WezTerm checks for these
  tools at startup and shows a warning notification if they are missing.

**Upload methods (SSH panes only):**
* **SFTP** — used when the pane belongs to a `RemoteSshDomain`
* **SCP** — used as fallback when SSH is detected via process inspection.
  SCP uses `BatchMode=yes`, so passwordless authentication (SSH key or agent)
  is required.

**Error handling:** If all fallbacks fail (no image and no text in clipboard,
or write/upload error), a toast notification is shown with the error message.

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

config.keys = {
  {
    key = 'V',
    mods = 'CTRL|SHIFT',
    action = act.PasteImageToSshUpload,
  },
}
```
