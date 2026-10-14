---
tags:
  - images
---

# Kitty Graphics Protocol

wezterm implements the [kitty graphics
protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/), which
allows programs to display images in the terminal by sending escape
sequences over the tty.

Use `enable_kitty_graphics = false` to disable the protocol.

{{since('20220101-133340-7edc5b5a')}} support for the protocol is
enabled by default.

## Supported features

| Area | Details |
| --- | --- |
| Transmission | `a=t`, `a=T`, chunked (`m=1`), direct (`t=d`), file (`t=f`), temp file (`t=t`) and shared memory (`t=s`) media |
| Formats | `f=32` RGBA, `f=24` RGB, `f=100` PNG |
| Compression | `o=z` (zlib/deflate) |
| Placement | source rectangle (`x=`, `y=`, `w=`, `h=`), pixel offsets (`X=`, `Y=`), cell sizing (`c=`, `r=`), z-index (`z=`), cursor policy (`C=`), multiple placements via `i=`/`p=` |
| Scaling | when only one of `c=`/`r=` is given the other is derived from the aspect ratio; with both, the image is scaled to fit and letterboxed rather than distorted |
| Z-index layering | negative values render beneath the text; values below `INT32_MIN/2` render beneath cells with a non-default background color; ties stack lower image id first |
| Deletion | all of `d=a/A`, `d=i/I`, `d=n/N`, `d=c/C`, `d=f/F`, `d=p/P`, `d=q/Q`, `d=r/R`, `d=x/X`, `d=y/Y`, `d=z/Z` |
| Unicode placeholders | `U=1` virtual placements rendered by `U+10EEEE` text; image id in the foreground color, placement id in the underline color, row/column in combining diacritics, with left-to-right inheritance |
| Relative placements | `P=`/`Q=`/`H=`/`V=` place an image relative to a parent placement; children move with their parent and are deleted with it (`ENOPARENT`, `ECYCLE`, `ETOODEEP`, `EINVAL` errors) |
| Animation | `a=f` frame transmission (including partial frames, base frame copying, gapless frames), `a=c` composition, `a=a` playback control (pause / loading mode / loop counts / client-driven frame selection / per-frame gaps), automatic playback |
| Query / errors | `a=q` probing; replies use the `OK` / `ENOENT:` / `EINVAL:` / `ENOSPC:` / `EBADF:` error codes and echo `i=`/`I=`/`p=` |

## Notes and limitations

* The shared memory medium (`t=s`) is not available on Android.
* Images are subject to a per-image RAM cap of 100MB and a global
  budget of 320MB; exceeding images are refused with `ENOSPC`.
* Image protocols are not fully handled by multiplexer sessions
  (ssh/mux domains) at this time; see
  [imgcat](imgcat.md) for the equivalent caveat for the iTerm2
  protocol.
