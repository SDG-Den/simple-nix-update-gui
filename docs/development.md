## Development

### Building

#### With Nix
```bash
# Build both packages
nix build .#daemon
nix build .#gui
nix build .#default  # builds GUI
```

#### With Cargo
```bash
# Build daemon
cd daemon && cargo build

# Build GUI
cd gui && cargo build
```

### Running tests

The flake defines checks for both packages. Run with:

```bash
nix flake check
```

Individual package builds in checks ensure the code compiles cleanly.

### Code formatting and linting

```bash
nix develop
cargo fmt --all
cargo clippy --all-targets --all-features
```

### Project dependencies

**Daemon**: serde, serde_json, zbus (D-Bus), notify-rust (desktop notifications), tracing, tokio, clap, chrono, dirs, hostname, anyhow

**GUI**: gtk4, libadwaita, vte4 (terminal), zbus, tokio, clap, serde, tracing, chrono, dirs, hostname, anyhow

**Runtime**: nix, nix-output-monitor (nom), polkit, systemd

## Troubleshooting

### Daemon not found by GUI

- Ensure the D-Bus session is running
- Check if daemon is running: `systemctl status simple-nix-update-gui-daemon` (if system service)
- The GUI falls back to local checks if D-Bus is unavailable

### Notifications don't appear

- For system service running as root, notifications need the user session's D-Bus and notification daemon running (e.g., dunst, mako, gnome-shell). The daemon uses `notify-rust` which connects to the user's session bus.
- Set `DBUS_SESSION_BUS_ADDRESS` appropriately or ensure it's inherited in the user context.

### Permission issues with rebuilds

- `nixos-rebuild` requires privileges. The GUI runs as the invoking user; use sudo/doas or rely on polkit rules configured by your system. The module sets up polkit rules allowing wheel group for power operations, but nixos-rebuild itself typically requires sudo elevation depending on your setup.
- A `git+` flake URI is cloned or pulled into the clone path (default `$HOME/repos/nix-config`) as the invoking user before `sudo` runs, so root never needs git credentials. If the clone or pull fails, its error appears in the integrated terminal and the rebuild does not start.
- Consider using `sudo` with NOPASSWD for specific commands if needed, or configure nix trusted users.

### Update detection seems incorrect

- Verify the flake URI and system name are correct
- Check that `nix eval` works: `nix eval --raw 'flake-uri#nixosConfigurations.systemName.system'`
- Ensure the flake is accessible (network access for remote flakes, proper paths)
- The current/remote paths are store paths; differences indicate the evaluated system hash differs from what's booted.

## License

This project is licensed under the GPL-3.0 License. See [LICENSE](../LICENSE) for details.

## Acknowledgements

This is a "vibe-coded" tool as noted in the README - built with the assistance of AI tooling. It uses standard NixOS, GTK4, and Rust ecosystem libraries.