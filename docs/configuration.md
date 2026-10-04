## Configuration

The application is configured via CLI arguments and environment variables. Configuration is shared between daemon and GUI.

### Daemon options

| CLI flag | Env var | Default | Description |
|---------|---------|---------|-------------|
| `--flake-uri` | `SNU_FLAKE_URI` | (required) | Flake URI to check. Any valid nix flake URI (e.g., `path:/etc/nixos`, `github:owner/repo`, `git+https://...`) |
| `--system-name` | `SNU_SYSTEM_NAME` | hostname | NixOS configuration name to check (e.g., `nixosConfigurations.<name>`) |
| `--check-interval` | `SNU_CHECK_INTERVAL` | `1h` | Check interval. Supports units: `s`, `m`, `min`, `h`, `d`, `hour`, etc. |
| `--auto-notify` | `SNU_AUTO_NOTIFY` | `true` | Send desktop notifications when updates become available |

### GUI options

| CLI flag | Env var | Default | Description |
|---------|---------|---------|-------------|
| `--flake-uri` | `SNU_FLAKE_URI` | `path:/etc/nixos` | Flake URI to use for rebuilds |
| `--system-name` | `SNU_SYSTEM_NAME` | hostname | System configuration name |
| `--use-nom` | `SNU_USE_NOM` | `true` | Pipe nixos-rebuild output through `nix-output-monitor` (nom) if available |

### NixOS module options

| Option | Type | Default | Description |
|-------|------|---------|-------------|
| `services.simple-nix-update-gui.enable` | bool | `false` | Enable the service |
| `services.simple-nix-update-gui.flakeUri` | str | - | Flake URI to monitor |
| `services.simple-nix-update-gui.systemName` | nullOr str | `null` | System name (defaults to `config.networking.hostName`) |
| `services.simple-nix-update-gui.checkInterval` | str | `"1h"` | Check interval for timer |
| `services.simple-nix-update-gui.autoNotify` | bool | `true` | Auto-send notifications |
| `services.simple-nix-update-gui.useNom` | bool | `true` | Use nom in integrated terminal |

## Behavior

1. **Update detection**: Compares the path of the currently booted system (`/run/booted-system`, symlink to a store path) with the evaluated system derivation for `flakeUri#nixosConfigurations.<systemName>.system`. If they differ, an update is available.
2. **Reboot detection**: Compares booted system against the current system profile (`/nix/var/nix/profiles/system`). If different, a reboot is needed to activate changes.
3. **Notifications**: When an update becomes available (and wasn't before), the daemon sends a desktop notification via `notify-rust`.
4. **Rebuilds**: The GUI launches `nixos-rebuild` with the chosen action (`build`, `boot`, or `switch`) in an embedded VTE terminal. Output can be piped through `nom` for better formatting.