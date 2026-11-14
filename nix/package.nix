{
  lib,
  stdenv,
  runCommand,

  # Native Build Inputs
  installShellFiles,
  ncurses,
  pkg-config,
  python3,
  # (Darwin only)
  perl,

  # Build inputs
  fontconfig,
  libgit2,
  openssl,
  sqlite,
  zlib,
  zstd,
  # (Linux only)
  libxkbcommon,
  wayland,
  libx11,
  libxcb,
  libxcb-util,
  libxcb-image,
  libxcb-keysyms,
  libxcb-wm,
  # (Darwin only)
  libiconv,

  # Rust (from rust-overlay)
  makeRustPlatform,
  rust-bin,

  # Runtime libs
  libGL,
  vulkan-loader,

  # Additionally injected stuff
  flakeInfo,
  inputSubmodules, # (packages from flake inputs)
}:

let
  nativeBuildInputs = [
    installShellFiles
    ncurses # tic for terminfo
    pkg-config
    python3
  ]
  ++ lib.optionals stdenv.hostPlatform.isDarwin [ perl ];

  buildInputs = [
    fontconfig
    libgit2
    openssl
    sqlite # Required by the sync-color-schemes workspace member
    zlib
    zstd
  ]
  ++ lib.optionals stdenv.hostPlatform.isLinux [
    libxkbcommon
    wayland

    libx11
    libxcb
    libxcb-util
    libxcb-image
    libxcb-keysyms
    libxcb-wm # contains xcb-ewmh among others
  ]
  ++ lib.optionals stdenv.hostPlatform.isDarwin ([
    libiconv
  ]);

  runtimeLibs = [
    libGL
    vulkan-loader
  ];

  rustPlatform = makeRustPlatform {
    cargo = rust-bin.stable.latest.minimal;
    rustc = rust-bin.stable.latest.minimal;
  };
in
rustPlatform.buildRustPackage (finalAttrs: {
  inherit buildInputs nativeBuildInputs;

  pname = "wezterm";
  src = ./..;

  # Rebuild the usual version number of the project from info of commit being built.
  # Format: `<date>-<time>-<shorthash>` (Example: `20200608-110940-3fb3a61`)
  # note: adds `-dirty` when the build includes uncomitted changes
  version =
    let
      # On a dirty tree `flakeInfo.rev` is null and only `flakeInfo.dirtyRev` is set.
      rev = if flakeInfo.rev != null then flakeInfo.rev else flakeInfo.dirtyRev;
    in
    builtins.concatStringsSep "-" (
      # note: `flakeInfo.lastModifiedDate` looks like `20240209045744`
      # So this match gives list with 2 items: `20240209` (date) & `045744` (time)
      builtins.match "(.{8})(.{6})" flakeInfo.lastModifiedDate
      ++ [ (builtins.substring 0 8 rev) ]
      ++ lib.lists.optional (flakeInfo.dirtyRev != null) "dirty"
    );

  cargoLock = {
    lockFile = ../Cargo.lock;
    allowBuiltinFetchGit = true;
  };

  # Inject collected submodules in-place for Nix build
  prePatch = ''
    rm -rf deps/freetype/{freetype2,libpng,zlib} deps/harfbuzz/harfbuzz

    ln -s ${inputSubmodules.freetype2} deps/freetype/freetype2
    ln -s ${inputSubmodules.libpng} deps/freetype/libpng
    ln -s ${inputSubmodules.zlib} deps/freetype/zlib
    ln -s ${inputSubmodules.harfbuzz} deps/harfbuzz/harfbuzz
  '';

  postPatch = ''
    echo ${finalAttrs.version} > .tag

    # tests are failing with: Unable to exchange encryption keys
    rm -r wezterm-ssh/tests

    # hash does not work well with NixOS
    substituteInPlace assets/shell-integration/wezterm.sh \
      --replace-fail 'hash wezterm 2>/dev/null' 'command type -P wezterm &>/dev/null' \
      --replace-fail 'hash base64 2>/dev/null' 'command type -P base64 &>/dev/null' \
      --replace-fail 'hash hostname 2>/dev/null' 'command type -P hostname &>/dev/null' \
      --replace-fail 'hash hostnamectl 2>/dev/null' 'command type -P hostnamectl &>/dev/null'
  '';

  # Disable cargo-auditable until https://github.com/rust-secure-code/cargo-auditable/issues/124 is fixed
  auditable = false;

  preFixup =
    lib.optionalString stdenv.hostPlatform.isLinux /* bash */ ''
      patchelf \
        --add-needed "${libGL}/lib/libEGL.so.1" \
        --add-needed "${vulkan-loader}/lib/libvulkan.so.1" \
        $out/bin/wezterm-gui
    ''
    + lib.optionalString stdenv.hostPlatform.isDarwin /* bash */ ''
      mkdir -p "$out/Applications"
      OUT_APP="$out/Applications/WezTerm.app"
      cp -r assets/macos/WezTerm.app "$OUT_APP"
      rm $OUT_APP/*.dylib
      cp -r assets/shell-integration/* "$OUT_APP/Contents/Resources"
      # macOS will only recognize our application bundle
      # if the binaries are inside of it. Move them there
      # and create symbolic links for them in bin/.
      mkdir -p "$OUT_APP/Contents/MacOS"
      mv $out/bin/{wezterm,wezterm-mux-server,wezterm-gui,strip-ansi-escapes} "$OUT_APP/Contents/MacOS"
      ln -s "$OUT_APP/Contents/MacOS"/{wezterm,wezterm-mux-server,wezterm-gui,strip-ansi-escapes} "$out/bin"
    '';

  preBuild = ''
    echo "Building Wezterm ${finalAttrs.version}..."
  '';

  postInstall = ''
    mkdir -p $out/nix-support
    echo "${finalAttrs.passthru.terminfo}" >> $out/nix-support/propagated-user-env-packages

    install -Dm644 assets/icon/terminal.png $out/share/icons/hicolor/128x128/apps/org.wezfurlong.wezterm.png
    install -Dm644 assets/wezterm.desktop $out/share/applications/org.wezfurlong.wezterm.desktop
    install -Dm644 assets/wezterm.appdata.xml $out/share/metainfo/org.wezfurlong.wezterm.appdata.xml

    install -Dm644 assets/shell-integration/wezterm.sh -t $out/etc/profile.d
    installShellCompletion --cmd wezterm \
      --bash assets/shell-completion/bash \
      --fish assets/shell-completion/fish \
      --zsh assets/shell-completion/zsh

    install -Dm644 assets/wezterm-nautilus.py -t $out/share/nautilus-python/extensions
  '';

  passthru = {
    # Runtime libs are only dlopened by the GUI.
    # A devshell cannot derive them from `inputsFrom`, so it reads them here.
    _runtimeLibs = runtimeLibs;

    # the headless variant is useful when deploying wezterm's mux server on remote severs
    headless = rustPlatform.buildRustPackage {
      pname = "wezterm-headless";
      inherit (finalAttrs)
        version
        src
        postPatch
        # inherit `cargoDeps` here because `buildRustPackage` strips `cargoLock`
        # from `finalAttrs` via `excludeDrvArgNames`
        cargoDeps
        meta
        ;

      nativeBuildInputs = [ pkg-config ];

      buildInputs = [
        libgit2
        openssl
        zstd
      ];

      cargoBuildFlags = [
        "--package"
        "wezterm"
        "--package"
        "wezterm-mux-server"
      ];

      doCheck = false;

      postInstall = ''
        install -Dm644 assets/shell-integration/wezterm.sh -t $out/etc/profile.d
        install -Dm644 ${finalAttrs.passthru.terminfo}/share/terminfo/w/wezterm -t $out/share/terminfo/w
      '';
    };

    terminfo =
      runCommand "wezterm-terminfo-${finalAttrs.version}"
        {
          nativeBuildInputs = [ ncurses ];
        }
        ''
          mkdir -p $out/share/terminfo $out/nix-support
          tic -x -o $out/share/terminfo ${finalAttrs.src}/termwiz/data/wezterm.terminfo
        '';
  };

  meta.mainProgram = "wezterm";
})
