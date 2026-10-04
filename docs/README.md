# Simple Nix Update GUI

A simple, desktop-friendly way to check for NixOS updates and perform rebuilds. This project consists of:

- **Notification daemon** (`simple-nix-update-gui-daemon`): Periodically checks for system updates via `nix eval` against a flake, sends desktop notifications when updates are available, and exposes state via D-Bus.
- **GUI application** (`simple-nix-update-gui`): GTK4/libadwaita interface that displays update status, allows running `nixos-rebuild` with different actions (build/boot/switch), and optionally pipes build output through `nix-output-monitor` (nom).
- **NixOS module**: Provides systemd service/timer configuration and polkit rules.

## Features

- D-Bus-based communication between daemon and GUI
- Periodic automatic update checking with configurable interval
- Desktop notifications via `notify-rust`
- Checks if a reboot is needed (compares `/run/booted-system` to current profile)
- Integrated terminal showing rebuild output (VTE)
- Optional `nix-output-monitor` (nom) support for nicer build logs
- Polkit integration for reboot actions
- Flake-aware (works with any flake URI)

## Project Structure

```
├── daemon/           # Rust daemon (D-Bus service)
│   ├── Cargo.toml
│   └── src/main.rs
├── gui/              # Rust GTK4 GUI
│   ├── Cargo.toml
│   └── src/main.rs
├── modules/          # NixOS module
│   └── nixos.nix
├── flake.nix         # Nix flake (packages, apps, modules, devShell)
├── LICENSE
└── README.md
```

## Architecture

The daemon runs as a systemd service (typically as root, as it needs to evaluate system flake state) and:
1. Checks current booted system against the evaluated system derivation from the configured flake (`flakeUri#nixosConfigurations.<systemName>.system`)
2. Maintains update state (has_update, current/remote paths, last check time)
3. Exposes D-Bus interface `org.simple_nix_update_gui.Daemon` at path `/org/simple_nix_update_gui/Daemon` with methods:
- `check_for_updates()` - triggers a manual check, returns JSON state
- `get_state()` - returns current state as JSON
- `is_reboot_needed()` - checks if booted system differs from profile

The GUI connects to the daemon's D-Bus service when available, falling back to local checks if the daemon isn't running. It provides a simple interface to view status and trigger rebuilds via `nixos-rebuild` in a terminal window.

## Requirements

- NixOS (or Nix with flake support)
- `nix` command available
- D-Bus session/system access as appropriate
- For GUI: GTK4, libadwaita, VTE (provided via Nix)
- For daemon: D-Bus, desktop notifications (libnotify)
