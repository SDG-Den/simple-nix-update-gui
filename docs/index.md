# Documentation Index

Welcome to the Simple Nix Update GUI documentation.

## Contents

- [Overview](README.md) - Project description, features, and architecture
- [Installation](installation.md) - How to install the project (NixOS module, manual, dev shell)
- [Configuration](configuration.md) - Configuration options for daemon, GUI, and NixOS module
- [Examples](examples/nixos-configuration.md) - Complete working example configurations
- [Development](development.md) - Building, testing, contributing, troubleshooting

## Quick Start

1. Add the flake to your NixOS configuration
2. Enable `services.simple-nix-update-gui.enable = true`
3. Set `flakeUri` to point to your system flake
4. Rebuild and switch
5. Launch the GUI from the launcher, or use the tray icon that autostarts with the session

For detailed setup instructions, see [Installation](installation.md). For configuration options, see [Configuration](configuration.md).