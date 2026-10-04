## Configuration

The application is configured via CLI arguments and environment variables. Configuration is shared between daemon and GUI.

### Daemon options

| CLI flag | Env var | Default | Description |
|---------|---------|---------|-------------|
| `--flake-uri` | `SNU_FLAKE_URI` | (required) | Flake URI to check. Any valid nix flake URI (e.g., `path:/etc/nixos`, `github:owner/repo`, `git+https://...`) |
| `--system-name` | `SNU_SYSTEM_NAME` | hostname | NixOS configuration name to check (e.g., `nixosConfigurations.<name>`) |
| `--check-interval` | `SNU_CHECK_INTERVAL` | `1h` | Check interval. Supports units: `s`, `m`, `min`, `h`, `d`, `hour`, etc. |
| `--bus-name` | `SNU_BUS_NAME` | `org.simple_nix_update_gui.Daemon` | Well known name the daemon owns on the system bus |

The daemon owns its bus name on the **system** bus and runs as root. There is no
`--auto-notify` option: the daemon holds no session, so it cannot raise notifications.
It runs the initial check immediately at startup and then keeps checking on
`--check-interval`, and systemd restarts it on failure.

### GUI options

| CLI flag | Env var | Default | Description |
|---------|---------|---------|-------------|
| `--flake-uri` | `SNU_FLAKE_URI` | `path:/etc/nixos` | Flake URI to use for rebuilds |
| `--system-name` | `SNU_SYSTEM_NAME` | hostname | System configuration name |
| `--use-nom` | `SNU_USE_NOM` | `true` | Pipe nixos-rebuild output through `nix-output-monitor` (nom) if available |
| `--auto-notify` | `SNU_AUTO_NOTIFY` | `true` | Send a desktop notification the first time an update is seen, and on every later `no update -> update` transition |
| `--check-interval` | `SNU_CHECK_INTERVAL` | `1h` | How often the GUI asks the daemon for state |
| `--bus-name` | `SNU_BUS_NAME` | `org.simple_nix_update_gui.Daemon` | Well known name to look for on the system bus |
| `--tray` | (none) | off | Start hidden with a status icon instead of showing the window |

Booleans are always written out explicitly by the NixOS module, including when
false, because both binaries fall back to `true` when the variable is absent. Use
`--use-nom=false` or `SNU_USE_NOM=false` when running by hand.

### NixOS module options

| Option | Type | Default | Description |
|-------|------|---------|-------------|
| `services.simple-nix-update-gui.enable` | bool | `false` | Enable the service |
| `services.simple-nix-update-gui.flakeUri` | str | - | Flake URI to monitor |
| `services.simple-nix-update-gui.systemName` | nullOr str | `null` | System name (defaults to `config.networking.hostName`) |
| `services.simple-nix-update-gui.busName` | str | `"org.simple_nix_update_gui.Daemon"` | System bus name used by the daemon and the GUI |
| `services.simple-nix-update-gui.checkInterval` | str | `"1h"` | Check interval, used by the daemon's own loop and by the GUI's poll interval |
| `services.simple-nix-update-gui.autoNotify` | bool | `true` | Let the GUI notify when an update becomes available |
| `services.simple-nix-update-gui.useNom` | bool | `true` | Use nom in integrated terminal |
| `services.simple-nix-update-gui.trayAutostart` | bool | `true` | Add the `xdg/autostart` entry that starts the GUI with `--tray` |

The module also registers the D-Bus service and policy files, so no manual
`dbus` configuration is needed.

## Behavior

1. **Update detection**: Compares the path of the currently booted system (`/run/booted-system`, symlink to a store path) with the evaluated system derivation for `flakeUri#nixosConfigurations.<systemName>.system`. If they differ, an update is available.
2. **Reboot detection**: Compares booted system against the current system profile (`/nix/var/nix/profiles/system`). If different, a reboot is needed to activate changes.
3. **Notifications**: The GUI sends a desktop notification via `notify-rust` the first time it sees an update, and again on each `no update -> update` transition. The daemon itself sends none.
4. **Rebuilds**: The GUI launches `nixos-rebuild` with the chosen action in an embedded VTE terminal. `build` runs unprivileged; `boot` and `switch` run through `pkexec`, so the desktop polkit agent asks for authentication instead of failing on a missing password prompt. Output can be piped through `nom` for better formatting.
5. **Tray**: With `--tray`, the GUI starts without a window, shows a status icon (Open, Check now, Quit), and closing the window hides it instead of exiting. Without `--tray`, closing the window exits, because there would be no icon to bring the window back.
6. **Daemon unavailable**: The GUI shows a banner, keeps working with its own `flakeUri`/`systemName`, and retries when the banner's Retry button is pressed.