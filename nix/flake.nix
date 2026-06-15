{
  description = "A GPU-accelerated cross-platform terminal emulator and multiplexer written by @wez and implemented in Rust";

  inputs = {
    self.submodules = true;

    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ self, ... }:
    inputs.flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import inputs.rust-overlay) ];
        pkgs = import (inputs.nixpkgs) { inherit system overlays; };

        inherit (inputs.nixpkgs) lib;
        inherit (pkgs) stdenv;

        nativeBuildInputs =
          with pkgs;
          [
            installShellFiles
            ncurses # tic for terminfo
            pkg-config
            python3
          ]
          ++ lib.optional stdenv.isDarwin perl;

        buildInputs =
          with pkgs;
          [
            fontconfig
            openssl
            zlib
          ]
          ++ lib.optionals stdenv.isLinux [
            libxkbcommon
            wayland

            libx11
            libxcb
            libxcb-util
            libxcb-image
            libxcb-keysyms
            libxcb-wm # contains xcb-ewmh among others
          ]
          ++ lib.optionals stdenv.isDarwin ([
            libiconv
          ]);

        libPath = lib.makeLibraryPath (
          with pkgs;
          [
            libxcb-image
            libGL
            vulkan-loader
          ]
        );

        rustPlatform = pkgs.makeRustPlatform {
          cargo = pkgs.rust-bin.stable.latest.minimal;
          rustc = pkgs.rust-bin.stable.latest.minimal;
        };
      in
      {
        packages.default = rustPlatform.buildRustPackage (finalAttrs: {
          inherit buildInputs nativeBuildInputs;

          pname = "wezterm";
          src = ./..;

          # Rebuild the usual version number of the project from info of commit being built.
          # Format: `<date>-<time>-<shorthash>` (Example: `20200608-110940-3fb3a61`)
          # note: adds `-dirty` when the build includes uncomitted changes
          version = (
            builtins.concatStringsSep "-" (
              # note: `self.lastModifiedDate` looks like `20240209045744`
              # So this match gives list with 2 items: `20240209` (date) & `045744` (time)
              builtins.match "(.{8})(.{6})" self.lastModifiedDate
              ++ [ (builtins.substring 0 8 self.rev or self.dirtyRev) ]
              ++ lib.lists.optional (self ? dirtyRev) "dirty"
            )
          );

          cargoLock = {
            lockFile = ../Cargo.lock;
            allowBuiltinFetchGit = true;
          };

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
            lib.optionalString stdenv.isLinux /* bash */ ''
              patchelf \
                --add-needed "${pkgs.libGL}/lib/libEGL.so.1" \
                --add-needed "${pkgs.vulkan-loader}/lib/libvulkan.so.1" \
                $out/bin/wezterm-gui
            ''
            + lib.optionalString stdenv.isDarwin /* bash */ ''
              mkdir -p "$out/Applications"
              OUT_APP="$out/Applications/WezTerm.app"
              cp -r assets/macos/WezTerm.app "$OUT_APP"
              rm $OUT_APP/*.dylib
              cp -r assets/shell-integration/* "$OUT_APP"
              # macOS will only recognize our application bundle
              # if the binaries are inside of it. Move them there
              # and create symbolic links for them in bin/.
              mv $out/bin/{wezterm,wezterm-mux-server,wezterm-gui,strip-ansi-escapes} "$OUT_APP"
              ln -s "$OUT_APP"/{wezterm,wezterm-mux-server,wezterm-gui,strip-ansi-escapes} "$out/bin"
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
            # the headless variant is useful when deploying wezterm's mux server on remote severs
            headless = rustPlatform.buildRustPackage {
              pname = "wezterm-headless";
              inherit (finalAttrs)
                version
                src
                postPatch
                cargoLock
                meta
                ;

              nativeBuildInputs = [ pkgs.pkg-config ];

              buildInputs = [ pkgs.openssl ];

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
              pkgs.runCommand "wezterm-terminfo-${finalAttrs.version}"
                {
                  nativeBuildInputs = [ pkgs.ncurses ];
                }
                ''
                  mkdir -p $out/share/terminfo $out/nix-support
                  tic -x -o $out/share/terminfo ${finalAttrs.src}/termwiz/data/wezterm.terminfo
                '';
          };

          meta.mainProgram = "wezterm";
        });

        devShell = pkgs.mkShell {
          name = "wezterm-shell";
          inherit nativeBuildInputs;

          buildInputs =
            buildInputs
            ++ (with pkgs.rust-bin; [
              (stable.latest.minimal.override {
                extensions = [
                  "clippy"
                  "rust-src"
                ];
              })

              nightly.latest.rustfmt
              nightly.latest.rust-analyzer
            ]);

          LD_LIBRARY_PATH = libPath;
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
