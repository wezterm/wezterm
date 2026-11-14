{
  description = "A GPU-accelerated cross-platform terminal emulator and multiplexer written by @wez and implemented in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # NOTE: @2026-06 Since Nix 2.27 we can set `inputs.self.submodules = true;` but it's silently
    # ignored for all usage of `github:…` flake refs..
    # refs:
    # - https://github.com/wezterm/wezterm/pull/7854#issuecomment-4718011398 (previous attempt)
    # - https://github.com/NixOS/nix/issues/13571
    # - https://github.com/NixOS/nix/issues/14982
    #
    # ... In the meantime we kinda duplicate the dependencies here then replace the submodules with
    # links to each repo in package sources.
    #
    # Try to use tags when possible to increase readability
    # (note: `git submodule status` in wezterm repo will show the `git describe` result for each
    # submodule, can help finding a tag if any)
    freetype2 = {
      url = "github:freetype/freetype/VER-2-13-3";
      flake = false;
    };
    harfbuzz = {
      url = "github:harfbuzz/harfbuzz/11.2.1";
      flake = false;
    };
    libpng = {
      url = "github:pnggroup/libpng/v1.6.44";
      flake = false;
    };
    zlib = {
      url = "github:madler/zlib/v1.3.1";
      flake = false;
    };
  };

  outputs =
    inputs@{ self, nixpkgs, ... }:
    let
      lib = nixpkgs.lib;

      systems = builtins.attrNames nixpkgs.legacyPackages;
      forSystems = lib.genAttrs systems;
      forSys = system: {
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import inputs.rust-overlay) ];
        };
        repopkgs = self.packages.${system};
      };

      vmTestingSystem = "x86_64-linux";
    in
    {
      packages = forSystems (
        system:
        let
          pkgs = (forSys system).pkgs;
          wezterm = pkgs.callPackage ./package.nix {
            flakeInfo = {
              inherit (self) lastModifiedDate;
              # note: we can't use `inherit` here because `rev` & `dirtyRev` may not be present.
              rev = self.rev or null;
              dirtyRev = self.dirtyRev or null;
            };
            inputSubmodules = {
              inherit (inputs)
                freetype2
                harfbuzz
                libpng
                zlib
                ;
            };
          };
        in
        {
          default = wezterm;
          inherit wezterm;
        }
      );

      devShells = forSystems (
        system: with (forSys system); {
          default = pkgs.mkShell {
            name = "wezterm-shell";

            # reuse the package build inputs instead of duplicating them
            inputsFrom = [ repopkgs.wezterm ];

            buildInputs =
              (with pkgs.rust-bin; [
                (stable.latest.minimal.override {
                  extensions = [
                    "clippy"
                    "rust-src"
                  ];
                })

                nightly.latest.rustfmt
                nightly.latest.rust-analyzer
              ])
              ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                # For local flatpak builds
                pkgs.flatpak-builder
                pkgs.flatpak-builder-tools # for flatpak-cargo-generator
                pkgs.appstream
              ];

            # `inputsFrom` does not provide a library search path.
            # The GUI dlopens libEGL and libvulkan at runtime, so we expose them in the shell.
            LD_LIBRARY_PATH = lib.makeLibraryPath repopkgs.wezterm._runtimeLibs;
          };
        }
      );

      formatter = forSystems (system: (forSys system).pkgs.nixfmt-tree);

      # 0. Build Wezterm locally: `cargo build`
      # 1. Build the VM: `nixos-rebuild build-vm --flake ./nix#testing-on-gnome`
      # 2. Start the VM: `REPO=$PWD ./result/bin/run-nixos-vm`
      # 3. (In the VM) (optional, 1-time) Adjust your keyboard layout in settings
      # 4. (In the VM) Open a terminal 'Console' on Gnome, 'Konsole' on KDE
      # 5. (In the VM) `cd ~/repo`
      # 6. (In the VM) Run the binary built! ``
      #
      # FIXME: cannot write to $REPO in the VM...
      nixosConfigurations.testing-on-gnome = import ./vm {
        inherit nixpkgs;
        system = vmTestingSystem;
        desktopModule = ./vm/desktop-gnome.nix;
      };
      nixosConfigurations.testing-on-plasma = import ./vm {
        inherit nixpkgs;
        system = vmTestingSystem;
        desktopModule = ./vm/desktop-plasma.nix;
      };
    };
}
