#!/usr/bin/env python3
"""Print versions of the bundled conhost binaries and the latest microsoft/terminal release (see README.md)."""

import argparse
import ctypes
import hashlib
import io
import re
import sys
import urllib.request
import zipfile
from ctypes import wintypes
from pathlib import Path

REPO = "microsoft/terminal"  # source named in README.md
here = Path(__file__).parent
# x64 files inside the release's ConPTY nupkg
NUPKG_FILES = {
    "build/native/runtimes/x64/OpenConsole.exe": "OpenConsole.exe",
    "runtimes/win-x64/native/conpty.dll": "conpty.dll",
}


def file_version(path) -> str | None:
    v = ctypes.windll.version
    size = v.GetFileVersionInfoSizeW(str(path), None)
    if not size:
        return None
    buf = ctypes.create_string_buffer(size)
    v.GetFileVersionInfoW(str(path), 0, size, buf)
    p, n = ctypes.c_void_p(), wintypes.UINT()
    if not v.VerQueryValueW(buf, "\\", ctypes.byref(p), ctypes.byref(n)):
        return None
    ms, ls = ctypes.cast(p, ctypes.POINTER(wintypes.DWORD * 4)).contents[2:4]
    return f"{ms >> 16}.{ms & 0xFFFF}.{ls >> 16}.{ls & 0xFFFF}"


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def latest_release():
    # the /releases/latest redirect avoids the GitHub API rate limit
    req = urllib.request.Request(
        f"https://github.com/{REPO}/releases/latest",
        headers={"User-Agent": "versions.py"},
    )
    with urllib.request.urlopen(req, timeout=15) as r:
        return r.geturl().rsplit("/", 1)[-1], r.geturl()


def get(url):
    with urllib.request.urlopen(
        urllib.request.Request(url, headers={"User-Agent": "versions.py"}), timeout=120
    ) as r:
        return r.read()


def download(tag):
    page = get(f"https://github.com/{REPO}/releases/expanded_assets/{tag}").decode()
    m = re.search(r'href="([^"]+ConPTY[^"]+\.nupkg)"', page)
    if not m:
        sys.exit("no ConPTY nupkg in release " + tag)
    z = zipfile.ZipFile(io.BytesIO(get("https://github.com" + m.group(1))))
    for src, name in NUPKG_FILES.items():
        (here / name).write_bytes(z.read(src))
        print(f"updated {name} -> {file_version(here / name)} {sha256(here / name)}")


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--download",
        action="store_true",
        help="replace the binaries with the latest release",
    )
    args = ap.parse_args()

    for f in sorted(here.glob("*.exe")) + sorted(here.glob("*.dll")):
        print(f"{f.name:16} {file_version(f):16} {sha256(f)}")
    tag, url = latest_release()
    print(f"\nlatest {REPO}: {tag} {url}")
    if args.download:
        download(tag)
