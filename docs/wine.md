# Building Windows with Wine

This is a local WSL/Linux setup for building `wezterm-gui.exe` with the
`x86_64-pc-windows-msvc` Rust target, using MSVC through Wine. It keeps the
Wine prefix and Windows Perl under `./wine/` so the setup does not affect the
rest of the system.

## Install MSVC for Wine

Install [msvc-wine](https://github.com/mstorsjo/msvc-wine) into this checkout:

```console
$ git clone https://github.com/mstorsjo/msvc-wine.git msvc-wine
$ cd msvc-wine
$ ./vsdownload.py --dest msvc
$ ./install.sh msvc
$ cd ..
```

The examples below assume the MSVC tools are available at
`./msvc-wine/msvc/bin/x64`.

## Install Portable Strawberry Perl

OpenSSL's Windows/MSVC build requires a Windows-style Perl. The system
`/usr/bin/perl` is not enough; OpenSSL will reject it because it does not
produce Windows paths.

Download the latest 64-bit portable Strawberry Perl ZIP from the
[Strawberry Perl releases repo](https://github.com/StrawberryPerl/Perl-Dist-Strawberry/releases)
into `./wine/`. Rename it to `strawberry-perl.zip`:

```console
$ mkdir -p wine/downloads wine/strawberry wine/bin wine/prefix
$ unzip -q wine/downloads/strawberry-perl.zip -d wine/strawberry
```

Create a repo-local `perl` wrapper that runs Strawberry Perl through Wine:

```console
$ cat > wine/bin/perl <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PERL_EXE="$(find "$ROOT/strawberry" -path '*/perl/bin/perl.exe' -print -quit)"

if [ -z "$PERL_EXE" ]; then
  echo "perl.exe not found under $ROOT/strawberry" >&2
  exit 1
fi

export WINEPREFIX="${WINEPREFIX:-$ROOT/prefix}"
export WINEARCH=win64

exec wine "$PERL_EXE" "$@"
EOF
$ chmod +x wine/bin/perl
```

Initialize the scoped Wine prefix and verify that the wrapper is first in
`PATH`:

```console
$ WINEPREFIX="$PWD/wine/prefix" WINEARCH=win64 wineboot -u
$ PATH="$PWD/wine/bin:$PATH" perl -v
```

Keep the local tool directories out of git:

```console
$ printf '\n/wine/\n/msvc-wine/\n' >> .git/info/exclude
```

## Build `wezterm-gui`

Install the Rust Windows MSVC target if needed:

```console
$ rustup target add x86_64-pc-windows-msvc
```

Build with the Wine Perl wrapper and MSVC wrapper tools for C/C++ dependencies,
but use Rust's `rust-lld` for the final Cargo link. The Wine MSVC `link.exe`
wrapper can get stuck at the final `wezterm-gui` link step; `rust-lld` avoids
that path, but needs explicit MSVC and Windows SDK library search paths:

```console
$ MSVC="$PWD/msvc-wine/msvc"
$ MSVC_TOOLS="$(find "$MSVC/vc/Tools/MSVC" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -1)"
$ SDK_VER="$(find "$MSVC/kits/10/Lib" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -1)"
$ RUST_HOST="$(rustc -vV | sed -n 's/^host: //p')"
$ RUST_LLD_DIR="$(rustc --print sysroot)/lib/rustlib/$RUST_HOST/bin"
$ PATH="$PWD/wine/bin:$MSVC/bin/x64:$RUST_LLD_DIR:$HOME/.cargo/bin:$PATH" \
  WINEPREFIX="$PWD/wine/prefix" \
  WINEARCH=win64 \
  WINE_MSVC_RAW_STDOUT=1 \
  VCINSTALLDIR="$MSVC/bin/x64" \
  VSINSTALLDIR="$MSVC/bin/x64" \
  CC_x86_64_pc_windows_msvc="$MSVC/bin/x64/cl" \
  CXX_x86_64_pc_windows_msvc="$MSVC/bin/x64/cl" \
  AR_x86_64_pc_windows_msvc="$MSVC/bin/x64/lib" \
  RC="$MSVC/bin/x64/rc" \
  RC_x86_64_pc_windows_msvc="$MSVC/bin/x64/rc" \
  CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER="rust-lld" \
  CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-Ctarget-feature=+crt-static -Clink-arg=/LIBPATH:$MSVC_TOOLS/atlmfc/lib/x64 -Clink-arg=/LIBPATH:$MSVC_TOOLS/lib/x64 -Clink-arg=/LIBPATH:$SDK_VER/ucrt/x64 -Clink-arg=/LIBPATH:$SDK_VER/um/x64 -Clink-arg=/LIBPATH:$SDK_VER/km/x64" \
  cargo build -p wezterm-gui --target x86_64-pc-windows-msvc --release
```

The output should be under:

```console
$ target/x86_64-pc-windows-msvc/release/wezterm-gui.exe
```

`VCINSTALLDIR` and `VSINSTALLDIR` are set to the wrapper directory so Rust
crates that call `find-msvc-tools` directly, such as `openssl-src`, can find
tools like `nmake.exe` while running on Linux. `WINE_MSVC_RAW_STDOUT=1` disables
the `msvctricks.exe` stdout helper; this avoids another Wine wrapper hang during
long native builds.

## Notes

The msvc-wine documentation shows CMake examples like:

```console
$ PATH="$HOME/Projects/wezterm/msvc-wine/msvc/bin/x64:$PATH" \
  CC=cl CXX=cl cmake ... -DCMAKE_BUILD_TYPE=Release -DCMAKE_SYSTEM_NAME=Windows
```

That pattern is useful for CMake projects, but WezTerm's `wezterm-gui` build is
driven by Cargo. The important replacement is to set Cargo's
`x86_64-pc-windows-msvc` compiler, archiver, resource compiler, and resource
compiler environment variables while using `rust-lld` plus explicit `/LIBPATH:`
arguments for the final link.
