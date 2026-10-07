{
  description = "Simple NixOS update GUI with per-user session bus state daemon";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {
          inherit system;
        };

        inherit (pkgs) lib;

        commonNativeBuildInputs = with pkgs; [
          pkg-config
          wrapGAppsHook4
        ];

        commonBuildInputs = with pkgs; [
          gtk4
          libadwaita
          vte-gtk4
          dbus
          glib
          polkit
          nix-output-monitor
        ];

        daemon = pkgs.rustPlatform.buildRustPackage {
          pname = "simple-nix-update-gui-daemon";
          version = "0.1.0";
          src = lib.cleanSource ./daemon;
          cargoLock.lockFile = ./daemon/Cargo.lock;
          nativeBuildInputs = with pkgs; [pkg-config];
          buildInputs = with pkgs; [dbus glib];
        };

        gui = pkgs.rustPlatform.buildRustPackage {
          pname = "simple-nix-update-gui";
          version = "0.1.0";
          src = lib.cleanSource ./gui;
          cargoLock.lockFile = ./gui/Cargo.lock;
          nativeBuildInputs = commonNativeBuildInputs ++ [pkgs.makeWrapper];
          buildInputs = commonBuildInputs;
          # The GUI shells out to these (rebuild terminal, store stats), so
          # they travel with the binary instead of depending on the session
          # that started it. nixos-rebuild is the rebuild entrypoint, and the
          # system profile is kept as a fallback so anything the user's normal
          # PATH exposes (sudo, systemctl post-sudo, and so on) stays
          # reachable. sudo must come from /run/wrappers/bin: the store copy
          # carries no setuid bit and cannot elevate.
          postInstall = ''
            wrapProgram $out/bin/simple-nix-update-gui \
              --prefix PATH : "${lib.makeBinPath (with pkgs; [nix git nix-output-monitor openssh coreutils util-linux bash nixos-rebuild])}:/run/wrappers/bin:/run/current-system/sw/bin"
          '';
        };

      in {
        packages = {
          inherit daemon gui;
          default = gui;
        };

        apps = {
          simple-nix-update-gui = {
            type = "app";
            program = "${gui}/bin/simple-nix-update-gui";
          };
          simple-nix-update-gui-daemon = {
            type = "app";
            program = "${daemon}/bin/simple-nix-update-gui-daemon";
          };
          default = self.apps.${system}.simple-nix-update-gui;
        };

        checks = {
          inherit daemon gui;
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
            pkg-config
          ];
          buildInputs = commonBuildInputs;
          RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
        };
      }
    )
    // {
      nixosModules.default = import ./modules/nixos.nix self;
      nixosModules.simple-nix-update-gui = import ./modules/nixos.nix self;
    };
}
