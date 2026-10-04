# Simple Nix Update GUI

A simple, desktop-friendly way to check for NixOS updates and perform rebuilds. This project consists of:

- **State daemon** (`simple-nix-update-gui-daemon`): Runs as root on the system bus, checks for system updates via `nix eval` against a flake on its own interval, and exposes state via D-Bus. It sends no notifications, because a root daemon has no session to raise them in.
- **GUI application** (`simple-nix-update-gui`): GTK4/libadwaita interface that displays update status, sends notifications, can hide itself in a status icon, allows running `nixos-rebuild` with different actions (build/boot/switch), and optionally pipes build output through `nix-output-monitor` (nom).
- **NixOS module**: Provides the systemd service, D-Bus service and policy files, launcher and autostart entries, and polkit rules.

## Features

- D-Bus-based communication over the system bus, with the daemon started on demand by `dbus`
- Periodic automatic update checking with configurable interval, no timer unit needed
- Desktop notifications via `notify-rust`, once per `no update -> update` transition
- Status icon (`ksni`) with Open, Check now, and Quit, and close-to-tray behavior
- Desktop launcher entry and `xdg/autostart` entry, both carrying every setting as a flag
- Checks if a reboot is needed (compares `/run/booted-system` to current profile)
- Integrated terminal showing rebuild output (VTE)
- `pkexec` for `nixos-rebuild boot`/`switch`, so a polkit agent can ask for the password
- Optional `nix-output-monitor` (nom) support for nicer build logs
- Banner in the window when the daemon cannot be reached
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

The daemon runs as a systemd service on the system bus as root, since it needs to
evaluate system flake state. It:
1. Checks current booted system against the evaluated system derivation from the configured flake (`flakeUri#nixosConfigurations.<systemName>.system`)
2. Maintains update state (has_update, current/remote paths, last check time)
3. Exposes D-Bus interface `org.simple_nix_update_gui.Daemon` at path `/org/simple_nix_update_gui/Daemon` with methods:
- `check_for_updates()` - triggers a manual check, returns JSON state
- `get_state()` - returns current state as JSON
- `is_reboot_needed()` - checks if booted system differs from profile

The GUI connects to that system bus name, polls `get_state()` on `checkInterval`, and
reacts to `check_for_updates()`. If the daemon is not reachable it shows a banner and
carries on with the flake URI and system name it was launched with. The GTK side runs
in a `glib` timeout that drains a channel fed by the Tokio tasks, so no widget is ever
touched off the main thread.

With `--tray` the GUI starts hidden, publishes a status icon, and holds the application
alive so that closing the window hides it instead of exiting.

## Requirements

- NixOS (or Nix with flake support)
- `nix` command available
- Read access to the system bus, and root for the daemon
- For GUI: GTK4, libadwaita (>= 1.4), VTE (provided via Nix), a StatusNotifierItem host
- For daemon: D-Bus only
