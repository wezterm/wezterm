chmod a+x Ghostty-${VERSION}-${ARCH}.appimage
./Ghostty-${VERSION}-${ARCH}.appimage
https://release.files.ghostty.org/VERSION/ghostty-VERSION.tar.gz
https://release.files.ghostty.org/VERSION/ghostty-VERSION.tar.gz.minisig
RWQlAjJC23149WL2sEpT/l0QKy7hMIFhYdQOFy0Z7z7PbneUgvlsnYcV
Can you refactor the function in @packages/functions/src/api/index.ts?We need to add authentication to the /settings route. Take a look at how this is
handled in the /notes route in @packages/functions/src/notes.ts and implement
the same logic in @packages/functions/src/settings.tsSounds good! Go ahead and make the changes.When a user deletes a note, we'd like to flag it as deleted in the database.
Then create a screen that shows all the recently deleted notes.
From this screen, the user can undelete a note or permanently delete it./initopencodecd /path/to/project┌ API key
│
│
└ enterdocker run -it --rm ghcr.io/anomalyco/opencodemise use -g github:anomalyco/opencodescoop install opencodechoco install opencode---
hide:
  - toc
---

*WezTerm is a powerful cross-platform terminal emulator and multiplexer written by <a href="https://github.com/wez/">@wez</a> and implemented in <a href="https://www.rust-lang.org/">Rust</a>*

![Screenshot](screenshots/wezterm-vday-screenshot.png)

[Download :material-tray-arrow-down:](installation.md){ .md-button }

## Features

* Runs on Linux, macOS, Windows 10, FreeBSD and NetBSD
* [Multiplex terminal panes, tabs and windows on local and remote hosts, with native mouse and scrollback](multiplexing.md)
* <a href="https://github.com/tonsky/FiraCode#fira-code-monospaced-font-with-programming-ligatures">Ligatures</a>, Color Emoji and font fallback, with true color and [dynamic color schemes](config/appearance.md).
* [Hyperlinks](hyperlinks.md)
* [A full list of features can be found here](features.md)

Looking for a [configuration reference?](config/files.md)

**These docs are searchable: press `S` or click on the magnifying glass icon
to activate the search function!**

<figure markdown>

![Screenshot](screenshots/two.png)

<figcaption>Screenshot of wezterm on macOS, running vim</figcaption>
</figure>
bun add -g opencode-aisudo pacman -S opencode           # Arch Linux (Stable)
paru -S opencode-bin              # Arch Linux (Latest from AUR)brew install anomalyco/tap/opencodecurl -fsSL https://opencode.ai/install | bashnpm install -g opencode-aiscoop install opencodechoco install opencode
