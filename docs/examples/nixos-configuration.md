## Example NixOS Configuration

Here is a complete, working example of using this project in a NixOS configuration:

### flake.nix

```nix
{
  description = "NixOS configuration with simple-nix-update-gui";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    simple-nix-update-gui = {
      url = "github:your-username/simple-nix-update-gui";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, simple-nix-update-gui, ... }: {
    nixosConfigurations.my-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        simple-nix-update-gui.nixosModules.default
      ];
    };
  };
}
```

### configuration.nix

```nix
{ config, pkgs, ... }: {
  # ... other configuration ...

  services.simple-nix-update-gui = {
    enable = true;
    # Point to your flake containing the system config
    flakeUri = "path:/etc/nixos";
    # Or use a remote flake
    # flakeUri = "github:your-username/nixos-config";
    systemName = "my-host";
    checkInterval = "2h"; # Check every 2 hours
    autoNotify = true;
    useNom = true;
  };

  # Ensure users in wheel group can use polkit for reboot (configured by module)
  # Also allow the daemon to notify users - notifications work in user session
  # The daemon runs as root but uses D-Bus to send notifications to the active user
}
```

### Environment variables example (manual use)

```bash
export SNU_FLAKE_URI="github:owner/nixos-configs"
export SNU_SYSTEM_NAME="desktop"
export SNU_CHECK_INTERVAL="30m"
export SNU_USE_NOM="true"
```

### Systemd user service example (optional alternative)

If you prefer running the daemon in user space, you can create a user systemd service. Note that checking system state still requires appropriate permissions; the root service as configured by the module is usually simpler.

```ini
# ~/.config/systemd/user/simple-nix-update-gui-daemon.service
[Unit]
Description=Simple Nix update GUI notification daemon
After=dbus.service

[Service]
Type=simple
Environment=SNU_FLAKE_URI=path:/etc/nixos
Environment=SNU_SYSTEM_NAME=%H
ExecStart=%h/.nix-profile/bin/simple-nix-update-gui-daemon
Restart=on-failure

[Install]
WantedBy=default.target
```

```bash
systemctl --user enable --now simple-nix-update-gui-daemon.service
systemctl --user enable --now simple-nix-update-gui-daemon.timer
```

(Requires creating a matching timer unit as well.)