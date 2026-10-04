{
  description = "Simple NixOS update GUI with notification daemon";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

    crane = {
      url = "github:ipetkov/crane";
    };

    flake-utils = {
      url = "github:numtide/flake-utils";
    };
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    flake-utils,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
      };

      craneLib = crane.mkLib pkgs;

      # Common build inputs for GTK/Rust
      commonNativeBuildInputs = with pkgs; [
        pkg-config
        wrapGAppsHook4
      ];

      commonBuildInputs = with pkgs; [
        gtk4
        libadwaita
        libvte-2.91-gtk4
        dbus
        glib
        polkit
        nix-output-monitor
      ];

      # Build daemon crate
      daemon = craneLib.buildPackage {
        pname = "simple-nix-update-gui-daemon";
        version = "0.1.0";
        src = craneLib.cleanCargoSource (./daemon);

        nativeBuildInputs = commonNativeBuildInputs;
        buildInputs = commonBuildInputs;

        cargoToml = ./daemon/Cargo.toml;
        cargoLock = ./daemon/Cargo.lock;
      };

      # Build GUI crate
      gui = craneLib.buildPackage {
        pname = "simple-nix-update-gui";
        version = "0.1.0";
        src = craneLib.cleanCargoSource (./gui);

        nativeBuildInputs = commonNativeBuildInputs;
        buildInputs = commonBuildInputs;

        cargoToml = ./gui/Cargo.toml;
        cargoLock = ./gui/Cargo.lock;
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
          pkg-config
        ];
        buildInputs = commonBuildInputs;
        RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
      };
    })
    // {
      nixosModules.default = import ./modules/nixos.nix;
      nixosModules.simple-nix-update-gui = import ./modules/nixos.nix;
    };
}
